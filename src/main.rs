use clap::{Args, Parser, Subcommand};
use mg_todo::{
    config::{Config, DatabaseUrl},
    domain::{Lifecycle, Project, ProjectId, Tag, TagId, Todo, TodoId, Version},
    human,
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
    /// Keep a reminder, resolving its identity, version, and timestamps
    Add(AddInput),
    /// List reminders, open ones by default
    Ls(ListInput),
    /// Complete one reminder by ID or unambiguous prefix
    Done(HandleInput),
    /// Trash one reminder by ID or unambiguous prefix
    Rm(HandleInput),
    /// Return one completed or trashed reminder to the open list
    Restore(HandleInput),
}

#[derive(Debug, Args)]
struct AddInput {
    /// What the reminder is
    title: String,
    /// today, tomorrow, YYYY-MM-DD, or YYYY-MM-DDTHH:MM
    #[arg(long)]
    due: Option<String>,
    /// IANA zone the due value is written in; defaults to the system zone
    #[arg(long)]
    timezone: Option<String>,
    /// Emit the stored domain object as JSON
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ListInput {
    /// Include completed and trashed reminders
    #[arg(long)]
    all: bool,
    /// Emit stored domain objects as JSON
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct HandleInput {
    /// The listed handle, or any unambiguous part of an identifier
    handle: String,
    /// Emit the stored domain object as JSON
    #[arg(long)]
    json: bool,
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
    #[error(transparent)]
    Human(#[from] human::HumanError),
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
        Command::Add(input) => add(PostgresTodoRepository::new(database_url), input).await,
        Command::Ls(input) => list(PostgresTodoRepository::new(database_url), input).await,
        Command::Done(input) => {
            close(
                PostgresTodoRepository::new(database_url),
                Lifecycle::Completed,
                input,
            )
            .await
        }
        Command::Rm(input) => {
            close(
                PostgresTodoRepository::new(database_url),
                Lifecycle::Trashed,
                input,
            )
            .await
        }
        Command::Restore(input) => reopen(PostgresTodoRepository::new(database_url), input).await,
    }
}

fn resolve<'a>(todos: &'a [Todo], token: &str) -> Result<&'a Todo, CliError> {
    let id = human::resolve_handle(todos, token)?;
    todos
        .iter()
        .find(|todo| todo.id() == id)
        .ok_or(CliError::NotFound {
            kind: "todo",
            id: token.to_owned(),
        })
}

async fn reopen(repository: PostgresTodoRepository, input: HandleInput) -> Result<(), CliError> {
    let todos = repository.list().await?;
    let current = resolve(&todos, &input.handle)?;
    let replacement = human::reopen(current, human::now())?;
    repository.replace(current.version(), &replacement).await?;
    if input.json {
        return print_json(&replacement);
    }
    println!("{}", human::render(&replacement));
    Ok(())
}

async fn add(repository: PostgresTodoRepository, input: AddInput) -> Result<(), CliError> {
    let at = human::now();
    let due = match input.due {
        None => None,
        Some(value) => {
            let zone = human::resolve_zone(input.timezone.as_deref())?;
            let today = human::today_in(&zone, at)?;
            Some(human::parse_due(&value, &zone, today)?)
        }
    };
    let todo = human::new_todo(input.title, due, at)?;
    repository.create(&todo).await?;
    if input.json {
        return print_json(&todo);
    }
    println!("{}", human::render(&todo));
    Ok(())
}

async fn list(repository: PostgresTodoRepository, input: ListInput) -> Result<(), CliError> {
    let mut todos = repository.list().await?;
    if !input.all {
        todos.retain(|todo| todo.lifecycle() == Lifecycle::Open);
    }
    if input.json {
        return print_json(&todos);
    }
    if todos.is_empty() {
        println!("no reminders");
        return Ok(());
    }
    // Undated reminders sort after dated ones, which is the order a day is read in
    todos.sort_by_key(|todo| (todo.due().is_none(), due_key(todo), todo.title().to_owned()));
    for todo in &todos {
        println!("{}", human::render(todo));
    }
    Ok(())
}

fn due_key(todo: &Todo) -> String {
    match todo.due() {
        None => String::new(),
        Some(mg_todo::domain::TodoDue::Date { date, .. }) => format!("{date}T00:00"),
        Some(mg_todo::domain::TodoDue::Timed { at, .. }) => at.to_rfc3339(),
    }
}

async fn close(
    repository: PostgresTodoRepository,
    lifecycle: Lifecycle,
    input: HandleInput,
) -> Result<(), CliError> {
    let todos = repository.list().await?;
    let current = resolve(&todos, &input.handle)?;
    let replacement = human::close(current, lifecycle, human::now())?;
    repository.replace(current.version(), &replacement).await?;
    if input.json {
        return print_json(&replacement);
    }
    println!("{}", human::render(&replacement));
    Ok(())
}

fn database_url(argument: Option<String>) -> Result<DatabaseUrl, CliError> {
    if let Some(value) = argument {
        return DatabaseUrl::parse(value).map_err(|_| CliError::Configuration);
    }
    Config::load()
        .map_err(|_| CliError::Configuration)?
        .database
        .database_url
        .ok_or(CliError::Configuration)
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
