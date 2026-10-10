//! `guide.provider`: the steps an agent walks a user through to register an OAuth app.
//! Rail returns commands as data; it never runs them.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::AccountKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GuideRequest {
    pub kind: AccountKind,
    #[serde(default)]
    pub inputs: GuideInputs,
}

/// Values filled into the steps. Anything absent stays as a `<placeholder>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GuideInputs {
    /// Google only: the Cloud project id to create or use.
    #[serde(default)]
    pub project_id: Option<String>,
    /// Microsoft only: the tenant id or domain.
    #[serde(default)]
    pub tenant: Option<String>,
    /// Microsoft only: who may sign in.
    #[serde(default)]
    pub audience: Option<Audience>,
    /// Microsoft only: the user administers the tenant and can grant admin consent.
    #[serde(default)]
    pub admin: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    /// One organisation ("Accounts in this organizational directory only").
    SingleTenant,
    /// Any organisation's work or school accounts.
    MultiTenant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GuideResponse {
    /// Bumped whenever the steps change.
    pub version: u32,
    pub kind: AccountKind,
    pub steps: Vec<GuideStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    /// A shell command for the agent to run, with the user's approval.
    Run,
    /// A page the user opens and works through themselves.
    Browser,
    /// A value to ask the user for.
    Ask,
    /// A Rail tool call.
    Rail,
    /// Text for the user to keep or publish; the step's `content`. Nothing is run or asked.
    Produce,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GuideStep {
    pub kind: StepKind,
    pub title: String,
    /// What to tell the user, in plain words.
    pub explanation: String,
    /// `run`: the command.
    pub command: Option<String>,
    /// `browser`: the page.
    pub url: Option<String>,
    /// `ask`: the value to ask for.
    pub ask: Option<String>,
    /// `rail`: the tool to call.
    pub tool: Option<String>,
    /// A command whose success means this step is already done.
    pub check: Option<String>,
    /// Text the step produces, such as a preset file.
    pub content: Option<String>,
    /// The step may be skipped.
    pub optional: bool,
}
