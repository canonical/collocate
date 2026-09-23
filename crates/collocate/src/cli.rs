use crate::output::{resolve_verbosity, Format, Verbosity};
use clap::{ArgAction, Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "collocate", version, about = "Run and manage lightweight containers.")]
#[command(
    after_help = "Containers:\n  run, list, status, start, stop, restart, exec, cp, logs, wait, delete\n\nCompose:\n  up, down, plan, config\n\nResources:\n  image, secret, load-balancer, cluster\n\nDiagnostics:\n  info, doctor\n\nRun 'collocate help <command>' for more information on a command."
)]
pub struct Cli {
    #[arg(long, global = true, default_value = "/run/collocate/collocate.sock", env = "COLLOCATE_HOST")]
    pub host: PathBuf,
    #[arg(long, global = true, value_enum, default_value_t = Format::Table)]
    pub format: Format,
    #[arg(short = 'q', long = "quiet", global = true, action = ArgAction::SetTrue)]
    pub quiet: bool,
    #[arg(long = "verbose", global = true, action = ArgAction::Count)]
    pub verbose: u8,
    #[arg(long = "verbosity", global = true, value_enum)]
    pub verbosity_override: Option<Verbosity>,
    #[arg(short = 'y', long = "yes", global = true)]
    pub yes: bool,
    #[arg(long, global = true, default_value = "/var/lib/collocate", env = "COLLOCATE_STATE_DIR")]
    pub state_dir: PathBuf,
    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    pub fn verbosity(&self) -> Verbosity {
        resolve_verbosity(self.verbosity_override, self.quiet, self.verbose)
    }
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  collocate run --series 24.04 -- /bin/myserver\n  collocate run -d --name web --memory 512m -p 8080:80 -- /usr/bin/myserver"
)]
pub struct RunArgs {
    #[arg(long)]
    pub series: Option<String>,
    #[arg(long, conflicts_with = "series")]
    pub image: Option<String>,
    #[arg(long)]
    pub entrypoint: Option<String>,
    #[arg(long)]
    pub publish_exposed: bool,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub hostname: Option<String>,
    #[arg(long, conflicts_with = "ephemeral")]
    pub persistent: bool,
    #[arg(long)]
    pub ephemeral: bool,
    #[arg(long)]
    pub cpus: Option<f64>,
    #[arg(long)]
    pub cpu_weight: Option<u32>,
    #[arg(long)]
    pub memory: Option<String>,
    #[arg(long)]
    pub swap: Option<String>,
    #[arg(long)]
    pub pids_max: Option<u64>,
    #[arg(short = 'v', long = "volume")]
    pub volume: Vec<String>,
    #[arg(short = 'p', long = "publish")]
    pub publish: Vec<String>,
    #[arg(short = 'e', long = "env")]
    pub env: Vec<String>,
    #[arg(long = "env-file")]
    pub env_file: Vec<PathBuf>,
    #[arg(short = 'u', long)]
    pub user: Option<String>,
    #[arg(short = 'w', long)]
    pub workdir: Option<String>,
    #[arg(long)]
    pub secret: Vec<String>,
    #[arg(long)]
    pub project: Option<String>,
    #[arg(long = "label")]
    pub label: Vec<String>,
    #[arg(long)]
    pub restart: Option<String>,
    #[arg(long)]
    pub tmpfs: Vec<String>,
    #[arg(long)]
    pub read_only: bool,
    #[arg(long)]
    pub cap_add: Vec<String>,
    #[arg(long)]
    pub cap_drop: Vec<String>,
    #[arg(long)]
    pub dns: Vec<String>,
    #[arg(long)]
    pub stop_signal: Option<String>,
    #[arg(long)]
    pub stop_timeout: Option<u64>,
    #[arg(short = 'd', long)]
    pub detach: bool,
    #[arg(last = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum SecretCmd {
    #[command(visible_alias = "ls")]
    List {
        project: Option<String>,
    },
    Set {
        project: String,
        name: String,
        #[arg(long)]
        from_file: Option<PathBuf>,
    },
    #[command(visible_alias = "rm")]
    Delete {
        project: String,
        name: String,
    },
    Reveal {
        project: String,
        name: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ImageCmd {
    Import {
        file: PathBuf,
    },
    #[command(visible_alias = "ls")]
    List,
    Show {
        name: String,
    },
    #[command(visible_alias = "rm")]
    Delete {
        name: String,
    },
    Prune,
}

#[derive(Debug, Args)]
pub struct ClusterArgs {
    #[arg(short = 'f', long, default_value = "collocate-compose.yaml")]
    pub file: PathBuf,
    #[arg(long, default_value = "lxc")]
    pub lxc: String,
    #[arg(long, default_value = "/usr/share/collocate/collocate.deb")]
    pub deb: String,
    #[arg(long, default_value = "collocate-relay")]
    pub relay: String,
    #[arg(long, default_value_t = 60)]
    pub timeout: u64,
}

#[derive(Debug, Subcommand)]
pub enum NodeCmd {
    Reinit,
    Adopt,
}

#[derive(Debug, Subcommand)]
pub enum ClusterCmd {
    Up(ClusterArgs),
    Status(ClusterArgs),
    #[command(visible_alias = "ls")]
    List {
        #[arg(long, default_value = "lxc")]
        lxc: String,
    },
}

#[derive(Debug, Args)]
pub struct LoadBalancerArgs {
    pub project: String,
    pub name: String,
    #[arg(long)]
    pub listen: u16,
    #[arg(long, default_value = "tcp")]
    pub proto: String,
    #[arg(long = "publish")]
    pub publish: Vec<u16>,
    #[arg(long)]
    pub backend_service: String,
    #[arg(long)]
    pub backend_port: u16,
    #[arg(long, default_value = "round-robin")]
    pub algorithm: String,
    #[arg(long, default_value = "reject")]
    pub on_no_backends: String,
    #[arg(long, default_value_t = 30)]
    pub drain: u64,
    #[arg(long)]
    pub vip: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum LoadBalancerCmd {
    #[command(visible_alias = "ls")]
    List,
    Show {
        project: String,
        name: String,
    },
    #[command(alias = "update")]
    Create(LoadBalancerArgs),
    #[command(visible_alias = "rm")]
    Delete {
        project: String,
        name: String,
    },
}

#[derive(Debug, Args)]
pub struct ComposeArgs {
    #[arg(short = 'f', long, default_value = "collocate-compose.yaml")]
    pub file: PathBuf,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long, default_value_t = 30)]
    pub timeout: u64,
    #[arg(long)]
    pub subnet: Option<String>,
    #[arg(long, num_args = 0..=1, default_missing_value = "")]
    pub regenerate_secrets: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Run(Box<RunArgs>),
    #[command(visible_alias = "ls", alias = "ps")]
    List {
        #[arg(short = 'a', long)]
        all: bool,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, value_delimiter = ',')]
        columns: Vec<String>,
        #[arg(long)]
        no_headers: bool,
        #[arg(long)]
        no_truncate: bool,
    },
    Status {
        target: Option<String>,
        #[arg(long, num_args = 0..=1, default_missing_value = "2")]
        watch: Option<u64>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        tree: bool,
        #[arg(long, value_delimiter = ',')]
        columns: Vec<String>,
        #[arg(long)]
        no_headers: bool,
        #[arg(long)]
        no_truncate: bool,
    },
    Stop {
        #[arg(required = true)]
        targets: Vec<String>,
        #[arg(short = 't', long)]
        timeout: Option<u64>,
    },
    Kill {
        #[arg(required = true)]
        targets: Vec<String>,
        #[arg(short = 's', long, default_value = "KILL")]
        signal: String,
    },
    #[command(visible_alias = "rm")]
    Delete {
        #[arg(required = true)]
        targets: Vec<String>,
        #[arg(short = 'f', long)]
        force: bool,
    },
    Start {
        #[arg(required = true)]
        targets: Vec<String>,
    },
    Restart {
        #[arg(required = true)]
        targets: Vec<String>,
        #[arg(short = 't', long)]
        timeout: Option<u64>,
    },
    Wait {
        target: String,
    },
    Logs {
        target: String,
        #[arg(short = 'f', long)]
        follow: bool,
        #[arg(long)]
        tail: Option<usize>,
    },
    Exec {
        #[arg(short = 'e', long = "env")]
        env: Vec<String>,
        #[arg(short = 'u', long)]
        user: Option<String>,
        #[arg(short = 'w', long)]
        workdir: Option<String>,
        target: String,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    Cp {
        src: String,
        dst: String,
    },
    #[command(subcommand)]
    Secret(SecretCmd),
    #[command(subcommand, alias = "lb")]
    LoadBalancer(LoadBalancerCmd),
    #[command(subcommand)]
    Image(ImageCmd),
    #[command(subcommand)]
    Cluster(ClusterCmd),
    #[command(subcommand)]
    Node(NodeCmd),
    Up(ComposeArgs),
    Down(ComposeArgs),
    Plan(ComposeArgs),
    Config {
        #[arg(short = 'f', long, default_value = "collocate-compose.yaml")]
        file: PathBuf,
        #[arg(long)]
        from_docker_compose: Option<PathBuf>,
        #[arg(long, default_value = "converted")]
        project: String,
    },
    Info,
    Doctor,
    Completion {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}
