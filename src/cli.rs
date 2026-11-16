use crate::client::Client;
use crate::commands::{auth, conversations, files, search, users};
use crate::config::{Config, Credentials};
use crate::error::{AppError, Result};
use crate::output::Output;
use clap::{Parser, Subcommand};

const LLM_GUIDE: &str = include_str!("llm.md");

#[derive(Parser)]
#[command(
    name = "slk",
    version,
    about = "Read-only, agent-friendly Slack CLI",
    after_help = "Output is JSON when stdout is not a terminal. Run `slk llm` for an agent guide."
)]
struct Cli {
    /// Workspace profile, team ID or team name
    #[arg(long, short = 'w', global = true, env = "SLK_WORKSPACE")]
    workspace: Option<String>,
    /// Force JSON output
    #[arg(long, global = true, conflicts_with = "text")]
    json: bool,
    /// Force human text output
    #[arg(long, global = true)]
    text: bool,
    /// Keep only these comma-separated fields (dot paths) of each listed item
    #[arg(long, global = true)]
    fields: Option<String>,
    /// Log API calls to stderr
    #[arg(long, short = 'v', global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage stored workspaces and credentials
    Auth {
        #[command(subcommand)]
        cmd: auth::AuthCmd,
    },
    /// List and read channels, DMs, threads and unreads
    #[command(visible_alias = "conv")]
    Conversations {
        #[command(subcommand)]
        cmd: conversations::ConvCmd,
    },
    /// Search messages, channels and people
    Search {
        #[command(subcommand)]
        cmd: search::SearchCmd,
    },
    /// Messages that @-mention you or your user groups
    Mentions {
        #[arg(long, default_value = "1d")]
        since: String,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// List files and print text-based ones
    Files {
        #[command(subcommand)]
        cmd: files::FilesCmd,
    },
    /// Read canvases as Markdown
    Canvas {
        #[command(subcommand)]
        cmd: files::CanvasCmd,
    },
    /// Read Slack Lists
    List {
        #[command(subcommand)]
        cmd: files::ListCmd,
    },
    /// Look up users
    Users {
        #[command(subcommand)]
        cmd: users::UsersCmd,
    },
    /// Print a usage guide written for LLM agents
    Llm,
    /// Call an allowlisted read method with key=value params and print the raw response
    #[command(hide = true)]
    Raw { method: String, params: Vec<String> },
}

pub struct Ctx {
    pub out: Output,
    pub workspace: Option<String>,
    pub verbose: bool,
}

impl Ctx {
    /// `--workspace` wins; otherwise an env token beats the stored default.
    pub fn client(&self) -> Result<Client> {
        if self.workspace.is_none()
            && let Some(token) = env_token()
        {
            return Client::new(Credentials::Standard { token }, self.verbose);
        }
        let (_, ws) = Config::load()?.select(self.workspace.as_deref())?;
        Client::new(ws.credentials, self.verbose)
    }
}

fn env_token() -> Option<String> {
    ["SLK_TOKEN", "SLACK_BOT_TOKEN"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
}

pub fn run() -> i32 {
    if std::env::args().nth(1).as_deref() == Some("--llm") {
        print!("{LLM_GUIDE}");
        return 0;
    }
    let cli = Cli::parse();
    let ctx = Ctx {
        out: Output::new(cli.json, cli.text, cli.fields),
        workspace: cli.workspace,
        verbose: cli.verbose,
    };
    let result: Result<()> = match cli.command {
        Command::Auth { cmd } => auth::run(&ctx, cmd),
        Command::Conversations { cmd } => conversations::run(&ctx, cmd),
        Command::Search { cmd } => search::run(&ctx, cmd),
        Command::Users { cmd } => users::run(&ctx, cmd),
        Command::Mentions { since, limit } => search::mentions(&ctx, &since, limit),
        Command::Files { cmd } => files::files(&ctx, cmd),
        Command::Canvas { cmd } => files::canvas(&ctx, cmd),
        Command::List { cmd } => files::list(&ctx, cmd),
        Command::Raw { method, params } => (|| {
            let params = params
                .iter()
                .map(|p| {
                    p.split_once('=')
                        .map(|(k, v)| (&*k.to_string().leak(), v.to_string()))
                        .ok_or_else(|| AppError::input(format!("param '{p}' is not key=value")))
                })
                .collect::<Result<Vec<_>>>()?;
            let r = ctx.client()?.call(&method, params)?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        })(),
        Command::Llm => {
            print!("{LLM_GUIDE}");
            Ok(())
        }
    };
    match result {
        Ok(()) => 0,
        Err(err) => {
            ctx.out.error(&err);
            err.exit_code()
        }
    }
}

pub fn read_limit(limit: usize, max: usize) -> Result<usize> {
    if limit == 0 || limit > max {
        return Err(AppError::input(format!(
            "--limit must be between 1 and {max}"
        )));
    }
    Ok(limit)
}
