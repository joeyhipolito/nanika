mod commands;
mod db;
mod dust_serve;
mod id;
mod import_linear;
mod models;
mod query;

use clap::{Parser, Subcommand};
use std::process;

#[derive(Parser)]
#[command(name = "tracker", about = "Local issue tracker", version)]
struct Cli {
    /// Path to the database file
    #[arg(long, env = "TRACKER_DB")]
    db: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new issue
    Create {
        /// Issue title
        title: String,
        /// Priority (P0, P1, P2, P3)
        #[arg(short, long)]
        priority: Option<String>,
        /// Description
        #[arg(short, long)]
        description: Option<String>,
        /// Assignee
        #[arg(short, long)]
        assignee: Option<String>,
        /// Comma-separated labels
        #[arg(short, long)]
        labels: Option<String>,
        /// Parent issue ID
        #[arg(long)]
        parent: Option<String>,
    },
    /// Show a single issue
    Show {
        /// Issue ID (e.g. trk-AB12)
        id: String,
    },
    /// List issues
    List {
        /// Filter by status
        #[arg(short, long)]
        status: Option<String>,
        /// Filter by priority
        #[arg(short, long)]
        priority: Option<String>,
    },
    /// Update an issue
    Update {
        /// Issue ID
        id: String,
        /// New title
        #[arg(short, long)]
        title: Option<String>,
        /// New status (open, in-progress, done, cancelled)
        #[arg(short, long)]
        status: Option<String>,
        /// New priority
        #[arg(short, long)]
        priority: Option<String>,
        /// New description
        #[arg(short, long)]
        description: Option<String>,
        /// New assignee
        #[arg(short, long)]
        assignee: Option<String>,
        /// New labels
        #[arg(short, long)]
        labels: Option<String>,
    },
    /// Delete an issue
    Delete {
        /// Issue ID
        id: String,
    },
    /// Link two issues
    Link {
        /// Source issue ID
        from: String,
        /// Target issue ID
        to: String,
        /// Link type: blocks, relates_to, supersedes, duplicates
        #[arg(short = 't', long, default_value = "relates_to")]
        link_type: String,
    },
    /// Remove a link between two issues
    Unlink {
        /// Source issue ID
        from: String,
        /// Target issue ID
        to: String,
        /// Link type to remove
        #[arg(short = 't', long, default_value = "relates_to")]
        link_type: String,
    },
    /// List open issues with no unresolved blocking links
    Ready,
    /// Show the highest-priority ready issue
    Next,
    /// Show issues as a parent–child tree
    Tree,
    /// Add a comment to an issue
    Comment {
        /// Issue ID
        id: String,
        /// Comment body
        body: String,
        /// Author name
        #[arg(long)]
        author: Option<String>,
    },
    /// Search issues by title or description
    Search {
        /// Search query
        query: String,
    },
    /// Serve the tracker as a Nanika dust dashboard plugin over a Unix socket
    DustServe,
    /// Import issues from Linear
    ImportLinear {
        /// Linear team identifier (e.g. NAN)
        #[arg(long, default_value = "NAN")]
        team: String,
    },
    /// Check tracker health and configuration
    Doctor {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Query plugin interface for nanika integration
    Query {
        /// Query subcommand (status, items, actions)
        subcommand: Option<String>,
        /// Additional arguments for the query
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

fn main() {
    let cli = Cli::parse();

    let db_path = match &cli.db {
        Some(p) => std::path::PathBuf::from(p),
        None => db::default_db_path(),
    };

    if let Err(e) = db::ensure_dir(&db_path) {
        eprintln!("error: failed to create database directory: {e}");
        process::exit(1);
    }

    let conn = match db::open(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: failed to open database at {}: {e}", db_path.display());
            process::exit(1);
        }
    };

    let result: Result<(), String> = match &cli.command {
        Commands::Create { title, priority, description, assignee, labels, parent } => {
            match commands::create(
                &conn,
                title,
                priority.as_deref(),
                description.as_deref(),
                assignee.as_deref(),
                labels.as_deref(),
                parent.as_deref(),
            ) {
                Ok(issue) => {
                    let display_id = issue.seq_id
                        .map(|n| format!("TRK-{}", n))
                        .unwrap_or_else(|| issue.id.clone());
                    println!("created {}", display_id);
                    commands::print_issue(&conn, &issue);
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        }

        Commands::Show { id } => match commands::get(&conn, id) {
            Ok(issue) => {
                commands::print_issue(&conn, &issue);
                Ok(())
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(format!("issue {id} not found")),
            Err(e) => Err(e.to_string()),
        },

        Commands::List { status, priority } => {
            match commands::list(&conn, status.as_deref(), priority.as_deref()) {
                Ok(issues) => {
                    commands::print_issues_table(&issues);
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        }

        Commands::Update { id, title, status, priority, description, assignee, labels } => {
            match commands::update(
                &conn,
                id,
                title.as_deref(),
                status.as_deref(),
                priority.as_deref(),
                description.as_deref(),
                assignee.as_deref(),
                labels.as_deref(),
            ) {
                Ok(issue) => {
                    let display_id = issue.seq_id
                        .map(|n| format!("TRK-{}", n))
                        .unwrap_or_else(|| issue.id.clone());
                    println!("updated {}", display_id);
                    commands::print_issue(&conn, &issue);
                    Ok(())
                }
                Err(rusqlite::Error::QueryReturnedNoRows) => Err(format!("issue {id} not found")),
                Err(e) => Err(e.to_string()),
            }
        }

        Commands::Delete { id } => match commands::delete(&conn, id) {
            Ok(()) => {
                println!("deleted {id}");
                Ok(())
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(format!("issue {id} not found")),
            Err(e) => Err(e.to_string()),
        },

        Commands::Link { from, to, link_type } => {
            match commands::link(&conn, from, to, link_type) {
                Ok(l) => {
                    println!("linked: {} --[{}]--> {}", l.from_id, l.link_type, l.to_id);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }

        Commands::Unlink { from, to, link_type } => {
            match commands::unlink(&conn, from, to, link_type) {
                Ok(()) => {
                    println!("unlinked: {} --[{}]--> {}", from, link_type, to);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }

        Commands::Ready => match commands::ready(&conn) {
            Ok(issues) => {
                if issues.is_empty() {
                    println!("No ready issues.");
                } else {
                    println!("{} ready issue(s):", issues.len());
                    commands::print_issues_table(&issues);
                }
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        },

        Commands::Next => match commands::next(&conn) {
            Ok(Some(issue)) => {
                println!("Next up:");
                commands::print_issue(&conn, &issue);
                Ok(())
            }
            Ok(None) => {
                println!("No ready issues.");
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        },

        Commands::Tree => commands::tree(&conn).map_err(|e| e.to_string()),

        Commands::Comment { id, body, author } => {
            match commands::comment(&conn, id, body, author.as_deref()) {
                Ok(c) => {
                    let author_str = c.author.as_deref().unwrap_or("anonymous");
                    println!("comment #{} added by {} on {}", c.id, author_str, c.issue_id);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }

        Commands::Search { query } => match commands::search(&conn, query) {
            Ok(issues) => {
                if issues.is_empty() {
                    println!("No results for {:?}.", query);
                } else {
                    println!("{} result(s):", issues.len());
                    commands::print_issues_table(&issues);
                }
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        },

        Commands::DustServe => {
            // The dust runtime owns the tokio runtime, so we exit after it
            // returns (either clean shutdown or I/O error).
            dust_serve::run(db_path).map_err(|e| e)
        }

        Commands::ImportLinear { team } => {
            import_linear::run(&conn, team).map_err(|e| e)
        }

        Commands::Doctor { json } => match commands::doctor(&conn, &db_path, *json) {
            Ok(output) => {
                println!("{}", output);
                Ok(())
            }
            Err(e) => Err(e),
        },

        Commands::Query { subcommand, args } => {
            match subcommand {
                Some(sub) => {
                    let mut query_args = vec![sub.clone()];
                    query_args.extend(args.clone());

                    // Check for --json flag
                    let has_json = query_args.iter().any(|a| a == "--json");

                    match query::query_cmd(&query_args, has_json) {
                        Ok(output) => {
                            println!("{}", output);
                            Ok(())
                        }
                        Err(e) => Err(e),
                    }
                }
                None => Err("query requires a subcommand: status, items, actions, or action".to_string()),
            }
        }
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        process::exit(1);
    }
}
