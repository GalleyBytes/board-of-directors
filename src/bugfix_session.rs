use crate::bugfix::SeverityLevel;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{RwLock, watch};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Starting,
    Waiting,
    Running,
    CancelRequested,
    Cancelled,
    Completed,
    TimedOut,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStep {
    Starting,
    Idle,
    Review,
    Consolidate,
    Fix,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SeverityCount {
    pub level: String,
    pub count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IterationActivityStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IterationActivity {
    pub id: String,
    pub actor: String,
    pub status: IterationActivityStatus,
    pub message: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionSnapshot {
    pub repo_name: String,
    pub branch: String,
    pub created_at_unix_secs: u64,
    pub started_at_unix_secs: u64,
    pub timeout_secs: u64,
    pub remaining_secs: u64,
    pub status: SessionStatus,
    pub current_step: SessionStep,
    pub current_step_label: String,
    pub iteration: u32,
    pub active_severity: SeverityLevel,
    pub next_severity: SeverityLevel,
    pub review_agents_total: u32,
    pub review_agents_completed: u32,
    pub review_agents_failed: u32,
    pub total_actionable: u32,
    pub severity_counts: Vec<SeverityCount>,
    pub iteration_activities: Vec<IterationActivity>,
    pub latest_message: String,
    pub latest_report_filename: Option<String>,
    pub latest_round_id: Option<String>,
    pub log_filename: String,
    pub cancel_requested: bool,
    pub will_revert_on_cancel: bool,
    pub last_error: Option<String>,
}

#[derive(Debug)]
struct SessionState {
    repo_name: String,
    branch: String,
    created_at_unix_secs: u64,
    started_at_unix_secs: u64,
    run_started_at_instant: Option<Instant>,
    timeout_secs: u64,
    status: SessionStatus,
    current_step: SessionStep,
    current_step_label: String,
    iteration: u32,
    active_severity: SeverityLevel,
    next_severity: SeverityLevel,
    review_agents_total: u32,
    review_agents_completed: u32,
    review_agents_failed: u32,
    total_actionable: u32,
    severity_counts: Vec<SeverityCount>,
    iteration_activities: Vec<IterationActivity>,
    latest_message: String,
    latest_report_filename: Option<String>,
    latest_round_id: Option<String>,
    log_filename: String,
    cancel_requested: bool,
    will_revert_on_cancel: bool,
    last_error: Option<String>,
}

#[derive(Debug)]
struct SharedSession {
    state_dir: PathBuf,
    sanitized_branch: String,
    review_codenames: Vec<String>,
    state: RwLock<SessionState>,
    cancel_tx: watch::Sender<bool>,
    start_tx: watch::Sender<bool>,
}

#[derive(Clone, Debug)]
pub struct BugfixSession {
    shared: Arc<SharedSession>,
}

impl BugfixSession {
    pub fn new(
        state_dir: PathBuf,
        repo_name: String,
        branch: String,
        sanitized_branch: String,
        review_codenames: Vec<String>,
        timeout_secs: u64,
        severity: SeverityLevel,
        log_filename: String,
    ) -> Self {
        let created_at_unix_secs = now_unix_secs();
        let (cancel_tx, _) = watch::channel(false);
        let (start_tx, _) = watch::channel(false);
        Self {
            shared: Arc::new(SharedSession {
                state_dir,
                sanitized_branch,
                review_codenames,
                state: RwLock::new(SessionState {
                    repo_name,
                    branch,
                    created_at_unix_secs,
                    started_at_unix_secs: created_at_unix_secs,
                    run_started_at_instant: None,
                    timeout_secs,
                    status: SessionStatus::Starting,
                    current_step: SessionStep::Starting,
                    current_step_label: "Preparing bugfix session".to_string(),
                    iteration: 0,
                    active_severity: severity,
                    next_severity: severity,
                    review_agents_total: 0,
                    review_agents_completed: 0,
                    review_agents_failed: 0,
                    total_actionable: 0,
                    severity_counts: Vec::new(),
                    iteration_activities: Vec::new(),
                    latest_message: "Preparing bugfix session".to_string(),
                    latest_report_filename: None,
                    latest_round_id: None,
                    log_filename,
                    cancel_requested: false,
                    will_revert_on_cancel: false,
                    last_error: None,
                }),
                cancel_tx,
                start_tx,
            }),
        }
    }

    pub fn state_dir(&self) -> PathBuf {
        self.shared.state_dir.clone()
    }

    pub fn sanitized_branch(&self) -> String {
        self.shared.sanitized_branch.clone()
    }

    pub fn review_codenames(&self) -> Vec<String> {
        self.shared.review_codenames.clone()
    }

    pub fn subscribe_cancel(&self) -> watch::Receiver<bool> {
        self.shared.cancel_tx.subscribe()
    }

    pub fn subscribe_start(&self) -> watch::Receiver<bool> {
        self.shared.start_tx.subscribe()
    }

    pub async fn request_cancel(&self) {
        let mut state = self.shared.state.write().await;
        state.cancel_requested = true;
        if matches!(
            state.status,
            SessionStatus::Starting | SessionStatus::Waiting | SessionStatus::Running
        ) {
            state.status = SessionStatus::CancelRequested;
        }
        if state.latest_message.is_empty() {
            state.latest_message = "Cancel requested".to_string();
        }
        let _ = self.shared.cancel_tx.send(true);
    }

    pub async fn is_cancel_requested(&self) -> bool {
        self.shared.state.read().await.cancel_requested
    }

    pub async fn mark_waiting_to_start(&self) {
        let mut state = self.shared.state.write().await;
        state.status = SessionStatus::Waiting;
        state.current_step = SessionStep::Starting;
        state.current_step_label = "Waiting to start bugfix session".to_string();
        state.latest_message =
            "Delayed start is enabled. Click Start in the dashboard or press Enter in the terminal."
                .to_string();
        state.last_error = None;
    }

    pub async fn request_start(&self) -> bool {
        let mut state = self.shared.state.write().await;
        if state.status != SessionStatus::Waiting {
            return false;
        }
        state.status = SessionStatus::Starting;
        state.current_step = SessionStep::Starting;
        state.current_step_label = "Starting bugfix session".to_string();
        state.latest_message =
            "Manual start received. Launching the first bugfix iteration...".to_string();
        let _ = self.shared.start_tx.send(true);
        true
    }

    pub async fn mark_run_started(&self) {
        let mut state = self.shared.state.write().await;
        state.started_at_unix_secs = now_unix_secs();
        state.run_started_at_instant = Some(Instant::now());
        if matches!(
            state.status,
            SessionStatus::Waiting | SessionStatus::Starting
        ) {
            state.status = SessionStatus::Starting;
            state.current_step = SessionStep::Starting;
            state.current_step_label = "Starting bugfix session".to_string();
        }
    }

    pub async fn snapshot(&self) -> SessionSnapshot {
        let state = self.shared.state.read().await;
        let elapsed = state
            .run_started_at_instant
            .as_ref()
            .map(|started_at| started_at.elapsed().as_secs())
            .unwrap_or(0);
        let remaining_secs = state.timeout_secs.saturating_sub(elapsed);
        SessionSnapshot {
            repo_name: state.repo_name.clone(),
            branch: state.branch.clone(),
            created_at_unix_secs: state.created_at_unix_secs,
            started_at_unix_secs: state.started_at_unix_secs,
            timeout_secs: state.timeout_secs,
            remaining_secs,
            status: state.status,
            current_step: state.current_step,
            current_step_label: state.current_step_label.clone(),
            iteration: state.iteration,
            active_severity: state.active_severity,
            next_severity: state.next_severity,
            review_agents_total: state.review_agents_total,
            review_agents_completed: state.review_agents_completed,
            review_agents_failed: state.review_agents_failed,
            total_actionable: state.total_actionable,
            severity_counts: state.severity_counts.clone(),
            iteration_activities: state.iteration_activities.clone(),
            latest_message: state.latest_message.clone(),
            latest_report_filename: state.latest_report_filename.clone(),
            latest_round_id: state.latest_round_id.clone(),
            log_filename: state.log_filename.clone(),
            cancel_requested: state.cancel_requested,
            will_revert_on_cancel: state.will_revert_on_cancel,
            last_error: state.last_error.clone(),
        }
    }

    pub async fn activate_iteration(
        &self,
        iteration: u32,
        label: impl Into<String>,
    ) -> SeverityLevel {
        let mut state = self.shared.state.write().await;
        state.iteration = iteration;
        state.active_severity = state.next_severity;
        state.status = SessionStatus::Running;
        state.current_step = SessionStep::Idle;
        state.current_step_label = label.into();
        state.review_agents_total = 0;
        state.review_agents_completed = 0;
        state.review_agents_failed = 0;
        state.total_actionable = 0;
        state.severity_counts.clear();
        state.iteration_activities.clear();
        state.will_revert_on_cancel = false;
        state.latest_message = format!(
            "Iteration {} started with {} severity threshold",
            iteration, state.active_severity
        );
        state.last_error = None;
        state.active_severity
    }

    pub async fn set_next_severity(&self, severity: SeverityLevel) {
        let mut state = self.shared.state.write().await;
        state.next_severity = severity;
        state.latest_message = format!("Next iteration severity set to {}", severity);
    }

    pub async fn set_message(&self, message: impl Into<String>) {
        self.shared.state.write().await.latest_message = message.into();
    }

    pub async fn begin_review(&self, total_agents: u32) {
        let review_codenames = self.shared.review_codenames.clone();
        let mut state = self.shared.state.write().await;
        state.current_step = SessionStep::Review;
        state.current_step_label = "Running multi-agent review".to_string();
        state.review_agents_total = total_agents;
        state.review_agents_completed = 0;
        state.review_agents_failed = 0;
        state.latest_round_id = None;
        state.latest_report_filename = None;
        state.latest_message = format!("Reviewing with {} agent(s)", total_agents);
        state
            .iteration_activities
            .retain(|activity| !activity.id.starts_with("review:"));
        for codename in review_codenames {
            set_iteration_activity(
                &mut state.iteration_activities,
                review_activity_id(&codename),
                codename.clone(),
                IterationActivityStatus::Running,
                format!("{} is reviewing...", codename),
                None,
            );
        }
    }

    pub async fn note_review_agent_result(&self, codename: &str, ok: bool, reason: Option<&str>) {
        let mut state = self.shared.state.write().await;
        if ok {
            state.review_agents_completed = state.review_agents_completed.saturating_add(1);
            state.latest_message = format!("Reviewer '{}' finished successfully", codename);
            set_iteration_activity(
                &mut state.iteration_activities,
                review_activity_id(codename),
                codename.to_string(),
                IterationActivityStatus::Completed,
                format!("{} finished review", codename),
                None,
            );
        } else {
            state.review_agents_failed = state.review_agents_failed.saturating_add(1);
            let detail = clean_activity_detail(codename, reason);
            state.latest_message = match detail.as_deref() {
                Some(detail) => format!("Reviewer '{}' failed: {}", codename, detail),
                None => format!("Reviewer '{}' failed", codename),
            };
            set_iteration_activity(
                &mut state.iteration_activities,
                review_activity_id(codename),
                codename.to_string(),
                IterationActivityStatus::Failed,
                format!("{} failed", codename),
                detail,
            );
        }
    }

    pub async fn finish_review_round(&self, round_id: &str) {
        let mut state = self.shared.state.write().await;
        state.latest_round_id = Some(round_id.to_string());
        state.latest_message = format!("Review round {} finished", round_id);
    }

    pub async fn begin_consolidation(&self, model: &str) {
        let mut state = self.shared.state.write().await;
        state.current_step = SessionStep::Consolidate;
        state.current_step_label = "Consolidating reviewer output".to_string();
        state.latest_message = format!("Consolidating reviews with {}", model);
        set_iteration_activity(
            &mut state.iteration_activities,
            consolidate_activity_id(model),
            model.to_string(),
            IterationActivityStatus::Running,
            format!("{} is consolidating reviews...", model),
            None,
        );
    }

    pub async fn set_latest_report(&self, filename: Option<String>) {
        let mut state = self.shared.state.write().await;
        state.latest_report_filename = filename;
    }

    pub async fn complete_consolidation(&self, model: &str) {
        let mut state = self.shared.state.write().await;
        set_iteration_activity(
            &mut state.iteration_activities,
            consolidate_activity_id(model),
            model.to_string(),
            IterationActivityStatus::Completed,
            format!("{} finished consolidation", model),
            None,
        );
    }

    pub async fn fail_consolidation(&self, model: &str, reason: impl Into<String>) {
        let reason = reason.into();
        let mut state = self.shared.state.write().await;
        state.latest_message = format!("Consolidation failed: {}", reason);
        set_iteration_activity(
            &mut state.iteration_activities,
            consolidate_activity_id(model),
            model.to_string(),
            IterationActivityStatus::Failed,
            format!("{} failed", model),
            clean_activity_detail(model, Some(reason.as_str())),
        );
    }

    pub async fn set_severity_counts(&self, counts: Vec<(String, u32)>, total_actionable: u32) {
        let mut state = self.shared.state.write().await;
        state.total_actionable = total_actionable;
        state.severity_counts = counts
            .into_iter()
            .map(|(level, count)| SeverityCount { level, count })
            .collect();
    }

    pub async fn begin_fix(&self, total_actionable: u32, model: &str) {
        let mut state = self.shared.state.write().await;
        state.current_step = SessionStep::Fix;
        state.current_step_label = "Applying fixes".to_string();
        state.total_actionable = total_actionable;
        state.latest_message = format!("Fixing {} issue(s) with {}", total_actionable, model);
        set_iteration_activity(
            &mut state.iteration_activities,
            fix_activity_id(model),
            model.to_string(),
            IterationActivityStatus::Running,
            format!("{} is fixing {} issue(s)...", model, total_actionable),
            None,
        );
    }

    pub async fn complete_fix(&self, model: &str) {
        let mut state = self.shared.state.write().await;
        set_iteration_activity(
            &mut state.iteration_activities,
            fix_activity_id(model),
            model.to_string(),
            IterationActivityStatus::Completed,
            format!("{} finished fixing", model),
            None,
        );
    }

    pub async fn fail_fix(&self, model: &str, reason: impl Into<String>) {
        let reason = reason.into();
        let mut state = self.shared.state.write().await;
        state.latest_message = format!("Fix step failed: {}", reason);
        set_iteration_activity(
            &mut state.iteration_activities,
            fix_activity_id(model),
            model.to_string(),
            IterationActivityStatus::Failed,
            format!("{} failed", model),
            clean_activity_detail(model, Some(reason.as_str())),
        );
    }

    pub async fn set_will_revert_on_cancel(&self, value: bool) {
        self.shared.state.write().await.will_revert_on_cancel = value;
    }

    pub async fn mark_cancelled(&self, message: impl Into<String>) {
        let message = message.into();
        let mut state = self.shared.state.write().await;
        state.status = SessionStatus::Cancelled;
        state.current_step = SessionStep::Finished;
        state.current_step_label = "Cancelled".to_string();
        mark_running_activities(
            &mut state.iteration_activities,
            IterationActivityStatus::Cancelled,
            "cancelled",
            Some(message.as_str()),
        );
        state.latest_message = message;
        state.will_revert_on_cancel = false;
    }

    pub async fn mark_completed(&self, message: impl Into<String>) {
        let message = message.into();
        let mut state = self.shared.state.write().await;
        if state.status == SessionStatus::Error || state.last_error.is_some() {
            state.status = SessionStatus::Error;
            state.current_step = SessionStep::Finished;
            state.current_step_label = "Error".to_string();
            state.will_revert_on_cancel = false;
            return;
        }
        state.status = SessionStatus::Completed;
        state.current_step = SessionStep::Finished;
        state.current_step_label = "Finished".to_string();
        state.latest_message = message;
        state.will_revert_on_cancel = false;
    }

    pub async fn mark_timed_out(&self, message: impl Into<String>) {
        let message = message.into();
        let mut state = self.shared.state.write().await;
        state.status = SessionStatus::TimedOut;
        state.current_step = SessionStep::Finished;
        state.current_step_label = "Timed out".to_string();
        mark_running_activities(
            &mut state.iteration_activities,
            IterationActivityStatus::Failed,
            "stopped",
            Some(message.as_str()),
        );
        state.latest_message = message;
        state.will_revert_on_cancel = false;
    }

    pub async fn mark_error(&self, error: impl Into<String>) {
        let error = error.into();
        let mut state = self.shared.state.write().await;
        state.status = SessionStatus::Error;
        state.current_step = SessionStep::Finished;
        state.current_step_label = "Error".to_string();
        mark_running_activities(
            &mut state.iteration_activities,
            IterationActivityStatus::Failed,
            "failed",
            Some(error.as_str()),
        );
        state.latest_message = error.clone();
        state.last_error = Some(error);
        state.will_revert_on_cancel = false;
    }
}

fn review_activity_id(codename: &str) -> String {
    format!("review:{}", codename)
}

fn consolidate_activity_id(model: &str) -> String {
    format!("consolidate:{}", model)
}

fn fix_activity_id(model: &str) -> String {
    format!("fix:{}", model)
}

fn set_iteration_activity(
    activities: &mut Vec<IterationActivity>,
    id: String,
    actor: String,
    status: IterationActivityStatus,
    message: String,
    detail: Option<String>,
) {
    if let Some(activity) = activities.iter_mut().find(|activity| activity.id == id) {
        activity.actor = actor;
        activity.status = status;
        activity.message = message;
        activity.detail = detail;
    } else {
        activities.push(IterationActivity {
            id,
            actor,
            status,
            message,
            detail,
        });
    }
}

fn clean_activity_detail(actor: &str, reason: Option<&str>) -> Option<String> {
    let trimmed = reason.map(str::trim).filter(|reason| !reason.is_empty())?;
    let stripped = trimmed
        .strip_prefix(actor)
        .and_then(|rest| rest.strip_prefix(' '))
        .unwrap_or(trimmed);
    Some(if stripped.starts_with("failed to ") {
        format!("it {}", stripped)
    } else {
        stripped.to_string()
    })
}

fn mark_running_activities(
    activities: &mut [IterationActivity],
    status: IterationActivityStatus,
    verb: &str,
    detail: Option<&str>,
) {
    let detail = detail
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
        .map(str::to_string);
    for activity in activities
        .iter_mut()
        .filter(|activity| activity.status == IterationActivityStatus::Running)
    {
        activity.status = status;
        activity.message = format!("{} {}", activity.actor, verb);
        activity.detail = detail.clone();
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn activate_iteration_uses_next_severity() {
        let session = BugfixSession::new(
            PathBuf::from("/tmp/state"),
            "repo".to_string(),
            "main".to_string(),
            "main".to_string(),
            vec!["opus".to_string()],
            60,
            SeverityLevel::High,
            "bugfix-main.log.md".to_string(),
        );
        session.set_next_severity(SeverityLevel::Low).await;
        let active = session.activate_iteration(2, "Iteration 2").await;

        assert_eq!(active, SeverityLevel::Low);
        let snapshot = session.snapshot().await;
        assert_eq!(snapshot.iteration, 2);
        assert_eq!(snapshot.active_severity, SeverityLevel::Low);
    }

    #[tokio::test]
    async fn request_cancel_updates_snapshot() {
        let session = BugfixSession::new(
            PathBuf::from("/tmp/state"),
            "repo".to_string(),
            "main".to_string(),
            "main".to_string(),
            vec!["opus".to_string()],
            60,
            SeverityLevel::High,
            "bugfix-main.log.md".to_string(),
        );

        session.request_cancel().await;
        let snapshot = session.snapshot().await;
        assert!(snapshot.cancel_requested);
        assert_eq!(snapshot.status, SessionStatus::CancelRequested);
    }

    #[tokio::test]
    async fn mark_waiting_to_start_sets_waiting_snapshot() {
        let session = BugfixSession::new(
            PathBuf::from("/tmp/state"),
            "repo".to_string(),
            "main".to_string(),
            "main".to_string(),
            vec!["opus".to_string()],
            60,
            SeverityLevel::High,
            "bugfix-main.log.md".to_string(),
        );

        session.mark_waiting_to_start().await;
        let snapshot = session.snapshot().await;
        assert_eq!(snapshot.status, SessionStatus::Waiting);
        assert_eq!(snapshot.current_step, SessionStep::Starting);
        assert_eq!(
            snapshot.current_step_label,
            "Waiting to start bugfix session"
        );
    }

    #[tokio::test]
    async fn request_start_promotes_waiting_session() {
        let session = BugfixSession::new(
            PathBuf::from("/tmp/state"),
            "repo".to_string(),
            "main".to_string(),
            "main".to_string(),
            vec!["opus".to_string()],
            60,
            SeverityLevel::High,
            "bugfix-main.log.md".to_string(),
        );

        session.mark_waiting_to_start().await;

        assert!(session.request_start().await);
        let snapshot = session.snapshot().await;
        assert_eq!(snapshot.status, SessionStatus::Starting);
        assert_eq!(snapshot.current_step_label, "Starting bugfix session");
    }

    #[tokio::test]
    async fn mark_run_started_preserves_creation_timestamp() {
        let session = BugfixSession::new(
            PathBuf::from("/tmp/state"),
            "repo".to_string(),
            "main".to_string(),
            "main".to_string(),
            vec!["opus".to_string()],
            60,
            SeverityLevel::High,
            "bugfix-main.log.md".to_string(),
        );

        let initial = session.snapshot().await;
        session.mark_run_started().await;
        let snapshot = session.snapshot().await;

        assert_eq!(snapshot.created_at_unix_secs, initial.created_at_unix_secs);
        assert!(snapshot.started_at_unix_secs >= snapshot.created_at_unix_secs);
    }

    #[tokio::test]
    async fn begin_review_clears_latest_round_and_report() {
        let session = BugfixSession::new(
            PathBuf::from("/tmp/state"),
            "repo".to_string(),
            "main".to_string(),
            "main".to_string(),
            vec!["opus".to_string()],
            60,
            SeverityLevel::High,
            "bugfix-main.log.md".to_string(),
        );

        session
            .finish_review_round("20260316153045n000000000001")
            .await;
        session
            .set_latest_report(Some(
                "20260316153045n000000000001-consolidated-main.md".to_string(),
            ))
            .await;

        session.begin_review(3).await;
        let snapshot = session.snapshot().await;
        assert_eq!(snapshot.latest_round_id, None);
        assert_eq!(snapshot.latest_report_filename, None);
    }

    #[tokio::test]
    async fn review_activities_persist_agent_progress() {
        let session = BugfixSession::new(
            PathBuf::from("/tmp/state"),
            "repo".to_string(),
            "main".to_string(),
            "main".to_string(),
            vec!["mini".to_string(), "haiku".to_string()],
            60,
            SeverityLevel::High,
            "bugfix-main.log.md".to_string(),
        );

        session.activate_iteration(1, "Iteration 1").await;
        session.begin_review(2).await;
        session.note_review_agent_result("mini", true, None).await;
        session
            .note_review_agent_result("haiku", false, Some("haiku failed to start agent: boom"))
            .await;

        let snapshot = session.snapshot().await;
        assert_eq!(snapshot.iteration_activities.len(), 2);
        assert_eq!(
            snapshot.iteration_activities[0].message,
            "mini finished review"
        );
        assert_eq!(snapshot.iteration_activities[1].message, "haiku failed");
        assert_eq!(
            snapshot.iteration_activities[1].detail.as_deref(),
            Some("it failed to start agent: boom")
        );
    }

    #[tokio::test]
    async fn consolidate_and_fix_activities_stay_visible_until_next_iteration() {
        let session = BugfixSession::new(
            PathBuf::from("/tmp/state"),
            "repo".to_string(),
            "main".to_string(),
            "main".to_string(),
            vec!["mini".to_string()],
            60,
            SeverityLevel::High,
            "bugfix-main.log.md".to_string(),
        );

        session.activate_iteration(1, "Iteration 1").await;
        session.begin_consolidation("gpt-5-mini").await;
        session.complete_consolidation("gpt-5-mini").await;
        session.begin_fix(3, "gpt-5-mini").await;
        session.complete_fix("gpt-5-mini").await;

        let snapshot = session.snapshot().await;
        assert_eq!(snapshot.iteration_activities.len(), 2);
        assert_eq!(
            snapshot.iteration_activities[0].message,
            "gpt-5-mini finished consolidation"
        );
        assert_eq!(
            snapshot.iteration_activities[1].message,
            "gpt-5-mini finished fixing"
        );

        session.activate_iteration(2, "Iteration 2").await;
        let next_snapshot = session.snapshot().await;
        assert!(next_snapshot.iteration_activities.is_empty());
    }

    #[tokio::test]
    async fn cancelling_marks_running_iteration_activity_cancelled() {
        let session = BugfixSession::new(
            PathBuf::from("/tmp/state"),
            "repo".to_string(),
            "main".to_string(),
            "main".to_string(),
            vec!["mini".to_string()],
            60,
            SeverityLevel::High,
            "bugfix-main.log.md".to_string(),
        );

        session.activate_iteration(1, "Iteration 1").await;
        session.begin_fix(2, "gpt-5-mini").await;
        session
            .mark_cancelled("Cancelled during the fix step.")
            .await;

        let snapshot = session.snapshot().await;
        assert_eq!(snapshot.iteration_activities.len(), 1);
        assert_eq!(
            snapshot.iteration_activities[0].status,
            IterationActivityStatus::Cancelled
        );
        assert_eq!(
            snapshot.iteration_activities[0].message,
            "gpt-5-mini cancelled"
        );
        assert_eq!(
            snapshot.iteration_activities[0].detail.as_deref(),
            Some("Cancelled during the fix step.")
        );
    }

    #[tokio::test]
    async fn mark_completed_preserves_existing_error_state() {
        let session = BugfixSession::new(
            PathBuf::from("/tmp/state"),
            "repo".to_string(),
            "main".to_string(),
            "main".to_string(),
            vec!["mini".to_string()],
            60,
            SeverityLevel::High,
            "bugfix-main.log.md".to_string(),
        );

        session.mark_error("fatal error").await;
        session
            .mark_completed("Iteration limit reached after 1 iteration(s).")
            .await;

        let snapshot = session.snapshot().await;
        assert_eq!(snapshot.status, SessionStatus::Error);
        assert_eq!(snapshot.current_step_label, "Error");
        assert_eq!(snapshot.last_error.as_deref(), Some("fatal error"));
        assert_eq!(snapshot.latest_message, "fatal error");
    }
}
