use clap::{Args, Parser, Subcommand};
use mg_remindr::{
    config::Config,
    domain::{Lifecycle, Project, ProjectId, Tag, TagId, Todo, TodoId, Version},
    human,
    storage::{ProjectRepository, Store, TagRepository, TodoRepository, migrate, migration_status},
};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    str::FromStr,
};
use thiserror::Error;

#[derive(Debug, Parser)]
#[command(name = "mg-remindr", about = "Local SQLite todo authority")]
struct Cli {
    /// The store file; defaults to `MG_REMINDR_DB` or config.toml
    #[arg(long, global = true, env = "MG_REMINDR_DB", hide_env_values = true)]
    db: Option<PathBuf>,
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
    /// Change one reminder's title and/or due value
    Edit(EditInput),
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
struct EditInput {
    /// The listed handle, or any unambiguous part of an identifier
    handle: String,
    /// Replace what the reminder is
    #[arg(long)]
    title: Option<String>,
    /// today, tomorrow, YYYY-MM-DD, or YYYY-MM-DDTHH:MM
    #[arg(long)]
    due: Option<String>,
    /// Remove the due value; refused together with --due
    #[arg(long)]
    clear_due: bool,
    /// IANA zone the due value is written in; defaults to the system zone
    #[arg(long)]
    timezone: Option<String>,
    /// Emit the stored domain object as JSON
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
    /// Export a complete validated mg-remindr snapshot
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
    Storage(#[from] mg_remindr::storage::StorageError),
    #[error("output serialization failed")]
    Output,
    #[error(transparent)]
    Human(#[from] human::HumanError),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mg-remindr: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), CliError> {
    let path = database_path(cli.db)?;
    // migration commands take the path itself: they report on a store they must not create
    match cli.command {
        Command::Migration { command } => match command {
            MigrationCommand::Status => print_json(&migration_status(&path)?),
            MigrationCommand::Apply => print_json(&migrate(&path)?),
        },
        Command::Project { command } => run_project(&path, command),
        Command::Tag { command } => run_tag(&path, command),
        Command::Todo { command } => run_todo(&path, command),
        Command::Interop { command } => match command {
            InteropCommand::Export => print_json(&mg_remindr::interop::export(&store(&path)?)?),
        },
        Command::Add(input) => add(&path, input),
        Command::Ls(input) => list(&path, &input),
        Command::Edit(input) => edit(&path, input),
        Command::Done(input) => close(&path, Lifecycle::Completed, &input),
        Command::Rm(input) => close(&path, Lifecycle::Trashed, &input),
        Command::Restore(input) => reopen(&path, &input),
    }
}

// Open the store, applying any migration it has not recorded
// Every command parses its input first and opens the store here, so unreadable input
// is reported without creating a store or touching an existing one
fn store(path: &Path) -> Result<Store, CliError> {
    Ok(Store::open(path)?)
}

// One repository each, over a store opened at the moment it is needed
fn projects(path: &Path) -> Result<ProjectRepository, CliError> {
    Ok(ProjectRepository::new(store(path)?))
}

fn tags(path: &Path) -> Result<TagRepository, CliError> {
    Ok(TagRepository::new(store(path)?))
}

fn todos(path: &Path) -> Result<TodoRepository, CliError> {
    Ok(TodoRepository::new(store(path)?))
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

fn reopen(path: &Path, input: &HandleInput) -> Result<(), CliError> {
    let repository = todos(path)?;
    let stored = repository.list()?;
    let current = resolve(&stored, &input.handle)?;
    let replacement = human::reopen(current, human::now())?;
    repository.replace(current.version(), &replacement)?;
    if input.json {
        return print_json(&replacement);
    }
    println!("{}", human::render(&replacement));
    Ok(())
}

fn add(path: &Path, input: AddInput) -> Result<(), CliError> {
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
    todos(path)?.create(&todo)?;
    if input.json {
        return print_json(&todo);
    }
    println!("{}", human::render(&todo));
    Ok(())
}

fn edit(path: &Path, input: EditInput) -> Result<(), CliError> {
    if input.due.is_some() && input.clear_due {
        return Err(CliError::Human(human::HumanError::ConflictingDue));
    }
    let repository = todos(path)?;
    let stored = repository.list()?;
    let current = resolve(&stored, &input.handle)?;
    let due = match (input.due, input.clear_due) {
        (Some(value), _) => {
            let at = human::now();
            let zone = human::resolve_zone(input.timezone.as_deref())?;
            let today = human::today_in(&zone, at)?;
            human::DueChange::Set(human::parse_due(&value, &zone, today)?)
        }
        (None, true) => human::DueChange::Clear,
        (None, false) => human::DueChange::Keep,
    };
    let replacement = human::amend(current, input.title, due, human::now())?;
    repository.replace(current.version(), &replacement)?;
    if input.json {
        return print_json(&replacement);
    }
    println!("{}", human::render(&replacement));
    Ok(())
}

fn list(path: &Path, input: &ListInput) -> Result<(), CliError> {
    let mut stored = todos(path)?.list()?;
    if !input.all {
        stored.retain(|todo| todo.lifecycle() == Lifecycle::Open);
    }
    if input.json {
        return print_json(&stored);
    }
    if stored.is_empty() {
        println!("no reminders");
        return Ok(());
    }
    // Undated reminders sort after dated ones, which is the order a day is read in
    stored.sort_by_key(|todo| (todo.due().is_none(), due_key(todo), todo.title().to_owned()));
    for todo in &stored {
        println!("{}", human::render(todo));
    }
    Ok(())
}

fn due_key(todo: &Todo) -> String {
    match todo.due() {
        None => String::new(),
        Some(mg_remindr::domain::TodoDue::Date { date, .. }) => format!("{date}T00:00"),
        Some(mg_remindr::domain::TodoDue::Timed { at, .. }) => at.to_rfc3339(),
    }
}

fn close(path: &Path, lifecycle: Lifecycle, input: &HandleInput) -> Result<(), CliError> {
    let repository = todos(path)?;
    let stored = repository.list()?;
    let current = resolve(&stored, &input.handle)?;
    let replacement = human::close(current, lifecycle, human::now())?;
    repository.replace(current.version(), &replacement)?;
    if input.json {
        return print_json(&replacement);
    }
    println!("{}", human::render(&replacement));
    Ok(())
}

// Where this run keeps its store: the argument, else the environment or config file
fn database_path(argument: Option<PathBuf>) -> Result<PathBuf, CliError> {
    if let Some(value) = argument {
        if value.as_os_str().is_empty() {
            return Err(CliError::Configuration);
        }
        return Ok(value);
    }
    Config::load()
        .map_err(|_| CliError::Configuration)?
        .database_path()
        .map_err(|_| CliError::Configuration)
}

fn run_project(path: &Path, command: ProjectCommand) -> Result<(), CliError> {
    match command {
        ProjectCommand::Create(input) => {
            let project = parse_json::<Project>(&input.json, "project")?;
            projects(path)?.create(&project)?;
            print_json(&project)
        }
        ProjectCommand::Find(input) => {
            let id = ProjectId::from_str(&input.id)
                .map_err(|_| CliError::InvalidId { kind: "project" })?;
            let project = projects(path)?.find(id)?.ok_or(CliError::NotFound {
                kind: "project",
                id: input.id,
            })?;
            print_json(&project)
        }
        ProjectCommand::List => print_json(&projects(path)?.list()?),
        ProjectCommand::Replace(input) => {
            let expected = version(input.expected_version)?;
            let project = parse_json::<Project>(&input.json, "project")?;
            projects(path)?.replace(expected, &project)?;
            print_json(&project)
        }
    }
}

fn run_tag(path: &Path, command: TagCommand) -> Result<(), CliError> {
    match command {
        TagCommand::Create(input) => {
            let tag = parse_json::<Tag>(&input.json, "tag")?;
            tags(path)?.create(&tag)?;
            print_json(&tag)
        }
        TagCommand::Find(input) => {
            let id = TagId::from_str(&input.id).map_err(|_| CliError::InvalidId { kind: "tag" })?;
            let tag = tags(path)?.find(id)?.ok_or(CliError::NotFound {
                kind: "tag",
                id: input.id,
            })?;
            print_json(&tag)
        }
        TagCommand::List => print_json(&tags(path)?.list()?),
        TagCommand::Replace(input) => {
            let expected = version(input.expected_version)?;
            let tag = parse_json::<Tag>(&input.json, "tag")?;
            tags(path)?.replace(expected, &tag)?;
            print_json(&tag)
        }
    }
}

fn run_todo(path: &Path, command: TodoCommand) -> Result<(), CliError> {
    match command {
        TodoCommand::Create(input) => {
            let todo = parse_json::<Todo>(&input.json, "todo")?;
            todos(path)?.create(&todo)?;
            print_json(&todo)
        }
        TodoCommand::Find(input) => {
            let id =
                TodoId::from_str(&input.id).map_err(|_| CliError::InvalidId { kind: "todo" })?;
            let todo = todos(path)?.find(id)?.ok_or(CliError::NotFound {
                kind: "todo",
                id: input.id,
            })?;
            print_json(&todo)
        }
        TodoCommand::List => print_json(&todos(path)?.list()?),
        TodoCommand::Replace(input) => {
            let expected = version(input.expected_version)?;
            let todo = parse_json::<Todo>(&input.json, "todo")?;
            todos(path)?.replace(expected, &todo)?;
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
