use clap::{Args, Parser, Subcommand};
use mg_todo::{
    config::{Config, DatabaseUrl},
    domain::{Project, ProjectId, Tag, TagId, Todo, TodoId, Version},
    storage::{
        PostgresProjectRepository, PostgresTagRepository, PostgresTodoRepository, migrate,
        migration_status,
    },
};
use serde::Serialize;
use std::{process::ExitCode, str::FromStr};
use thiserror::Error;

#[derive(Debug, Parser)]
#[command(name = "mg-todo", about = "Local PostgreSQL todo authority")]
struct Cli {
    /// Local PostgreSQL URL; defaults to `MG_TODO_DATABASE_URL` or config.toml
    #[arg(
        long,
        global = true,
        env = "MG_TODO_DATABASE_URL",
        hide_env_values = true
    )]
    database_url: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Migration {
        #[command(subcommand)]
        command: MigrationCommand,
    },
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    Tag {
        #[command(subcommand)]
        command: TagCommand,
    },
    Todo {
        #[command(subcommand)]
        command: TodoCommand,
    },
    Interop {
        #[command(subcommand)]
        command: InteropCommand,
    },
}

#[derive(Debug, Subcommand)]
enum InteropCommand {
    /// Export a complete validated mg-todo snapshot
    Export,
}

#[derive(Debug, Subcommand)]
enum MigrationCommand {
    /// Show embedded migration state
    Status,
    /// Apply all pending embedded migrations
    Apply,
}

#[derive(Debug, Subcommand)]
enum ProjectCommand {
    /// Create a project from a JSON object
    Create(JsonInput),
    /// Find a project by UUID
    Find(IdInput),
    /// List projects in stable UUID order
    List,
    /// Replace a project using optimistic version authority
    Replace(ReplaceInput),
}

#[derive(Debug, Subcommand)]
enum TagCommand {
    /// Create a tag from a JSON object
    Create(JsonInput),
    /// Find a tag by UUID
    Find(IdInput),
    /// List tags in stable UUID order
    List,
    /// Replace a tag using optimistic version authority
    Replace(ReplaceInput),
}

#[derive(Debug, Subcommand)]
enum TodoCommand {
    /// Create a core todo from a JSON object
    Create(JsonInput),
    /// Find a todo by UUID
    Find(IdInput),
    /// List core todos in stable UUID order
    List,
    /// Replace a core todo using optimistic version authority
    Replace(ReplaceInput),
}

#[derive(Debug, Args)]
struct JsonInput {
    /// Complete domain object encoded as JSON
    #[arg(long)]
    json: String,
}

#[derive(Debug, Args)]
struct IdInput {
    id: String,
}

#[derive(Debug, Args)]
struct ReplaceInput {
    /// Currently persisted version
    #[arg(long)]
    expected_version: u64,
    /// Complete replacement domain object encoded as JSON
    #[arg(long)]
    json: String,
}

#[derive(Debug, Error)]
enum CliError {
    #[error("database URL is required (--database-url, MG_TODO_DATABASE_URL, or config.toml)")]
    MissingDatabaseUrl,
    #[error("invalid database configuration")]
    Configuration,
    #[error("invalid {kind} JSON")]
    InvalidJson { kind: &'static str },
    #[error("invalid {kind} identifier")]
    InvalidId { kind: &'static str },
    #[error("invalid expected version")]
    InvalidVersion,
    #[error("{kind} {id} was not found")]
    NotFound { kind: &'static str, id: String },
    #[error(transparent)]
    Storage(#[from] mg_todo::storage::StorageError),
    #[error("output serialization failed")]
    Output,
    #[error("runtime initialization failed")]
    Runtime,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        eprintln!("mg-todo: {}", CliError::Runtime);
        return ExitCode::FAILURE;
    };
    match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mg-todo: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), CliError> {
    let database_url = database_url(cli.database_url)?;
    match cli.command {
        Command::Migration { command } => match command {
            MigrationCommand::Status => print_json(&migration_status(&database_url).await?),
            MigrationCommand::Apply => print_json(&migrate(&database_url).await?),
        },
        Command::Project { command } => {
            run_project(PostgresProjectRepository::new(database_url), command).await
        }
        Command::Tag { command } => {
            run_tag(PostgresTagRepository::new(database_url), command).await
        }
        Command::Todo { command } => {
            run_todo(PostgresTodoRepository::new(database_url), command).await
        }
        Command::Interop { command } => match command {
            InteropCommand::Export => print_json(&mg_todo::interop::export(&database_url).await?),
        },
    }
}

fn database_url(argument: Option<String>) -> Result<DatabaseUrl, CliError> {
    if let Some(value) = argument {
        return DatabaseUrl::parse(value).map_err(|_| CliError::Configuration);
    }
    Config::load()
        .map_err(|_| CliError::Configuration)?
        .database
        .database_url
        .ok_or(CliError::MissingDatabaseUrl)
}

async fn run_project(
    repository: PostgresProjectRepository,
    command: ProjectCommand,
) -> Result<(), CliError> {
    match command {
        ProjectCommand::Create(input) => {
            let project = parse_json::<Project>(&input.json, "project")?;
            repository.create(&project).await?;
            print_json(&project)
        }
        ProjectCommand::Find(input) => {
            let id = ProjectId::from_str(&input.id)
                .map_err(|_| CliError::InvalidId { kind: "project" })?;
            let project = repository.find(id).await?.ok_or(CliError::NotFound {
                kind: "project",
                id: input.id,
            })?;
            print_json(&project)
        }
        ProjectCommand::List => print_json(&repository.list().await?),
        ProjectCommand::Replace(input) => {
            let expected = version(input.expected_version)?;
            let project = parse_json::<Project>(&input.json, "project")?;
            repository.replace(expected, &project).await?;
            print_json(&project)
        }
    }
}

async fn run_tag(repository: PostgresTagRepository, command: TagCommand) -> Result<(), CliError> {
    match command {
        TagCommand::Create(input) => {
            let tag = parse_json::<Tag>(&input.json, "tag")?;
            repository.create(&tag).await?;
            print_json(&tag)
        }
        TagCommand::Find(input) => {
            let id = TagId::from_str(&input.id).map_err(|_| CliError::InvalidId { kind: "tag" })?;
            let tag = repository.find(id).await?.ok_or(CliError::NotFound {
                kind: "tag",
                id: input.id,
            })?;
            print_json(&tag)
        }
        TagCommand::List => print_json(&repository.list().await?),
        TagCommand::Replace(input) => {
            let expected = version(input.expected_version)?;
            let tag = parse_json::<Tag>(&input.json, "tag")?;
            repository.replace(expected, &tag).await?;
            print_json(&tag)
        }
    }
}

async fn run_todo(
    repository: PostgresTodoRepository,
    command: TodoCommand,
) -> Result<(), CliError> {
    match command {
        TodoCommand::Create(input) => {
            let todo = parse_json::<Todo>(&input.json, "todo")?;
            repository.create(&todo).await?;
            print_json(&todo)
        }
        TodoCommand::Find(input) => {
            let id =
                TodoId::from_str(&input.id).map_err(|_| CliError::InvalidId { kind: "todo" })?;
            let todo = repository.find(id).await?.ok_or(CliError::NotFound {
                kind: "todo",
                id: input.id,
            })?;
            print_json(&todo)
        }
        TodoCommand::List => print_json(&repository.list().await?),
        TodoCommand::Replace(input) => {
            let expected = version(input.expected_version)?;
            let todo = parse_json::<Todo>(&input.json, "todo")?;
            repository.replace(expected, &todo).await?;
            print_json(&todo)
        }
    }
}

fn parse_json<T: serde::de::DeserializeOwned>(
    value: &str,
    kind: &'static str,
) -> Result<T, CliError> {
    serde_json::from_str(value).map_err(|_| CliError::InvalidJson { kind })
}

fn version(value: u64) -> Result<Version, CliError> {
    Version::try_from_value(value).map_err(|_| CliError::InvalidVersion)
}

fn print_json<T: Serialize>(value: &T) -> Result<(), CliError> {
    println!(
        "{}",
        serde_json::to_string(value).map_err(|_| CliError::Output)?
    );
    Ok(())
}
