mod github_api;

use crate::github_api::Milestone;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use structopt::StructOpt;
use tokio::prelude::*;

mod settings {
    #[derive(serde::Deserialize, Default)]
    pub struct Root {
        pub github: GitHub,
    }

    #[derive(serde::Deserialize, Default)]
    pub struct GitHub {
        #[serde(default)]
        pub repositories: Vec<String>,
        #[serde(default)]
        pub auth: Option<Authentication>,
    }

    #[derive(serde::Deserialize)]
    pub struct Authentication {
        pub username: String,
        pub token: String,
    }
}

#[derive(StructOpt)]
enum Commands {
    #[structopt(name = "close-milestone")]
    /// Close the given milestone for all configured repositories
    CloseMilestone {
        /// A regular expression matching against the milestone name
        pattern: regex::Regex,
    },
}

#[derive(Debug)]
enum Error {
    NoConfigDir,
    InvalidConfigFile(config::ConfigError),
    Reqwest(reqwest::Error),
    AuthRequired,
    IO(std::io::Error),
}

struct RepositoryMilestones {
    repository: String,
    milestones: Vec<Milestone>,
}

fn main() -> Result<(), Error> {
    let project_dir =
        directories::ProjectDirs::from("tech", "coblox", "GH CLI").ok_or(Error::NoConfigDir)?;

    let config_file = project_dir.config_dir().join("settings.toml");

    println!(
        "Reading configuration file from {}",
        config_file
            .to_str()
            .expect("path to config file should be a printable string")
    );

    let settings = if config_file.exists() {
        let mut config = config::Config::default();
        config
            .merge(config::File::from(config_file))
            .expect("config should not be frozen");

        config
            .try_into::<settings::Root>()
            .map_err(Error::InvalidConfigFile)?
    } else {
        println!("Config file not found - continuing with defaults");
        settings::Root::default()
    };

    let mut runtime = tokio::runtime::Runtime::new().expect("should be able to get a runtime");

    let command = Commands::from_args();

    match command {
        Commands::CloseMilestone { pattern } => {
            let settings::Root {
                github: settings::GitHub { repositories, auth },
            } = settings;

            let settings::Authentication { username, token } = auth.ok_or(Error::AuthRequired)?;
            let client = reqwest::r#async::Client::new();

            let matching_milestones = {
                let username = username.clone();
                let token = token.clone();
                let client = client.clone();

                let repository_milestones: Vec<RepositoryMilestones> = runtime
                    .block_on(future::join_all(repositories.into_iter().map(
                        move |repo| {
                            client
                                .clone()
                                .get(&format!("https://api.github.com/repos/{}/milestones", repo))
                                .header("Accept", "application/vnd.github.v3+json")
                                .basic_auth(username.clone(), Some(token.clone()))
                                .send()
                                .and_then(|mut response| {
                                    let repo_clone = repo.clone();

                                    response
                                        .json::<Vec<github_api::Milestone>>()
                                        .or_else(move |_| {
                                            eprintln!(
                                                "Request to {} failed with statuscode {}",
                                                repo_clone,
                                                response.status().as_u16()
                                            );
                                            Ok(Vec::new())
                                        })
                                        .map(move |milestones| RepositoryMilestones {
                                            repository: repo.clone(),
                                            milestones,
                                        })
                                })
                        },
                    )))
                    .map_err(Error::Reqwest)?;

                repository_milestones.into_iter().fold(
                    HashMap::new(),
                    |mut map,
                     RepositoryMilestones {
                         repository,
                         milestones,
                     }| {
                        for milestone in milestones {
                            if !pattern.is_match(&milestone.title) {
                                continue;
                            }

                            match map.entry(milestone.title) {
                                Entry::Vacant(vacant) => {
                                    vacant.insert(vec![(milestone.url, repository.clone())]);
                                }
                                Entry::Occupied(mut occupied) => {
                                    occupied.get_mut().push((milestone.url, repository.clone()));
                                }
                            }
                        }

                        map
                    },
                )
            };

            println!();
            println!(
                "Found {} open milestones matching the pattern '{}':",
                matching_milestones.len(),
                pattern
            );

            for (index, (milestone, repositories)) in matching_milestones.into_iter().enumerate() {
                println!("({}) '{}' is open in:", index + 1, milestone);
                for (_, repository) in &repositories {
                    println!(" - {}", repository);
                }
                println!();

                if dialoguer::Confirmation::new()
                    .with_text(&format!(
                        "Close milestone '{}' in those repositories?",
                        milestone
                    ))
                    .interact()
                    .map_err(Error::IO)?
                {
                    let username = username.clone();
                    let token = token.clone();
                    let client = client.clone();

                    runtime
                        .block_on(future::join_all(
                            repositories
                                .into_iter()
                                .map(move |(url, repo)| {
                                    client
                                        .clone()
                                        .patch(&url)
                                        .header("Accept", "application/vnd.github.v3+json")
                                        .basic_auth(username.clone(), Some(token.clone()))
                                        .json(&serde_json::json!({
                                            "state": "closed"
                                        }))
                                        .send()
                                        .and_then(move |response| {
                                            if !response.status().is_success() {
                                                eprintln!("Failed to close milestone for repository {}", repo);
                                            }
                                            Ok(())
                                        })
                                })
                                .collect::<Vec<_>>(),
                        ))
                        .map_err(Error::Reqwest)?;
                }
            }
        }
    }

    Ok(())
}
