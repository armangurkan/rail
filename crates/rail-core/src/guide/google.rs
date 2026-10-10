//! Google: `gcloud` creates the project and enables the Gmail API (flags checked against
//! Google Cloud SDK 586.0.0), but it cannot create the desktop OAuth client Gmail needs (its
//! only OAuth-client commands are for Identity-Aware Proxy), so those are browser steps.

use super::step;
use crate::ops::{GuideStep, StepKind};

pub(super) fn steps() -> Vec<GuideStep> {
    vec![
        login(),
        create_project(),
        use_project(),
        enable_api(),
        consent_screen(),
        desktop_client(),
        find_file(),
        add_account(),
        preset(),
    ]
}

fn login() -> GuideStep {
    GuideStep {
        command: Some("gcloud auth login".into()),
        check: Some(
            "gcloud auth list --filter=status:ACTIVE --format=\"value(account)\" | grep -q ."
                .into(),
        ),
        ..step(
            StepKind::Run,
            "Sign in to Google Cloud",
            "Signs gcloud in to the user's Google account. A browser page opens for them.",
        )
    }
}

fn create_project() -> GuideStep {
    GuideStep {
        command: Some("gcloud projects create <project-id>".into()),
        check: Some("gcloud projects describe <project-id>".into()),
        ..step(
            StepKind::Run,
            "Create a project",
            "A Cloud project holds the OAuth client. The id must be globally unique, 6 to 30 lowercase letters, digits or hyphens.",
        )
    }
}

fn use_project() -> GuideStep {
    GuideStep {
        command: Some("gcloud config set project <project-id>".into()),
        ..step(
            StepKind::Run,
            "Use the project",
            "Makes the new project the default for the next commands.",
        )
    }
}

fn enable_api() -> GuideStep {
    GuideStep {
        command: Some("gcloud services enable gmail.googleapis.com".into()),
        check: Some(
            "gcloud services list --enabled --format=\"value(config.name)\" | grep -qx gmail.googleapis.com"
                .into(),
        ),
        ..step(
            StepKind::Run,
            "Enable the Gmail API",
            "Rail reads and sends mail through the Gmail API.",
        )
    }
}

fn consent_screen() -> GuideStep {
    GuideStep {
        url: Some(
            "https://console.cloud.google.com/apis/credentials/consent?project=<project-id>".into(),
        ),
        ..step(
            StepKind::Browser,
            "Set up the consent screen",
            "The user takes over in the browser: choose External, fill in the app name and their address, then publish the app so its status is \"In production\". An app left in Testing expires refresh tokens after 7 days, and Rail would keep asking them to log in. Google shows an \"unverified app\" warning for their own app; that is expected.",
        )
    }
}

fn desktop_client() -> GuideStep {
    GuideStep {
        url: Some("https://console.cloud.google.com/apis/credentials?project=<project-id>".into()),
        ..step(
            StepKind::Browser,
            "Create a desktop client",
            "The user takes over in the browser: Create credentials, OAuth client ID, application type Desktop app, then download the JSON file it offers. They should not paste the client secret anywhere.",
        )
    }
}

fn find_file() -> GuideStep {
    GuideStep {
        ask: Some("the path of the downloaded client_secret_*.json file".into()),
        ..step(
            StepKind::Ask,
            "Find the downloaded file",
            "Usually in the Downloads folder, named client_secret_<id>.json.",
        )
    }
}

fn add_account() -> GuideStep {
    GuideStep {
        tool: Some("rail_account_add".into()),
        content: Some(
            "{\"kind\": \"gmail\", \"address\": \"<address>\", \"client_secret_file\": \"<path>\"}"
                .into(),
        ),
        ..step(
            StepKind::Rail,
            "Add the account",
            "Call rail_account_add with the user's address and the file path. Show the user the URL it returns, then call rail_wait with its id while they log in.",
        )
    }
}

fn preset() -> GuideStep {
    GuideStep {
        content: Some(
            "name = \"<organisation> Gmail\"\nkind = \"gmail\"\nclient_id = \"<client-id>\"\nclient_secret = \"<client-secret>\"\n"
                .into(),
        ),
        optional: true,
        ..step(
            StepKind::Produce,
            "Publish a preset (administrators)",
            "If an administrator wants colleagues to skip these steps, this is the preset file to publish over https or in /etc/rail/presets. The administrator fills in the client id and secret from the JSON file themselves; Rail never writes presets.",
        )
    }
}
