use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::Mutex;
use tokio_cron_scheduler::{JobScheduler, Job};
use hydragent_types::CronJob;
use sqlx::SqlitePool;
use uuid::Uuid;
use anyhow::{Context, Result};
use std::future::Future;
use std::pin::Pin;

pub type JobExecutor = Arc<
    dyn Fn(CronJob) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static
>;

pub struct CronScheduler {
    inner: JobScheduler,
    job_handles: Mutex<HashMap<String, uuid::Uuid>>,
    pool: SqlitePool,
    executor: JobExecutor,
}

impl CronScheduler {
    pub async fn new(pool: SqlitePool, executor: JobExecutor) -> Result<Arc<Self>> {
        let scheduler = JobScheduler::new().await
            .context("Failed to create JobScheduler")?;
        scheduler.start().await.context("Failed to start JobScheduler")?;

        let this = Arc::new(Self {
            inner: scheduler,
            job_handles: Mutex::new(HashMap::new()),
            pool,
            executor,
        });

        // Reload active jobs from SQLite on startup
        this.reload_from_db().await?;

        Ok(this)
    }

    /// Register a new cron job. Persists to SQLite and schedules it.
    pub async fn add_job(
        &self,
        cron_expr: &str,
        description: &str,
        task_type: &str,
        task_params: &str,
        target_channel_id: &str,
    ) -> Result<String> {
        let job_id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp_millis();

        // Validate cron expression
        cron_expr.parse::<cron::Schedule>()
            .with_context(|| format!("Invalid cron expression: '{}'", cron_expr))?;

        let cron_job = CronJob {
            id: job_id.clone(),
            cron_expr: cron_expr.to_string(),
            description: description.to_string(),
            task_type: task_type.to_string(),
            task_params: task_params.to_string(),
            target_channel_id: target_channel_id.to_string(),
            status: "active".to_string(),
            created_at: now,
            last_run_at: None,
            run_count: 0,
        };

        // Persist to database first
        sqlx::query(
            "INSERT INTO cron_jobs (id, cron_expr, description, task_type, task_params, target_channel_id, status, created_at)
             VALUES (?, ?, ?, ?, ?, ?, 'active', ?)"
        )
        .bind(&cron_job.id)
        .bind(&cron_job.cron_expr)
        .bind(&cron_job.description)
        .bind(&cron_job.task_type)
        .bind(&cron_job.task_params)
        .bind(&cron_job.target_channel_id)
        .bind(now)
        .execute(&self.pool)
        .await?;

        // Schedule in memory
        self.schedule_in_memory(cron_job).await?;

        Ok(job_id)
    }

    async fn schedule_in_memory(&self, job_def: CronJob) -> Result<()> {
        let executor = self.executor.clone();
        let pool = self.pool.clone();
        let job_id = job_def.id.clone();
        let cron_expr = job_def.cron_expr.clone();

        let job_id_for_closure = job_id.clone();
        // Create async job closure
        let job = Job::new_async(cron_expr.as_str(), move |_uuid, _l| {
            let executor = executor.clone();
            let pool = pool.clone();
            let job_def = job_def.clone();
            let job_id = job_id_for_closure.clone();
            Box::pin(async move {
                tracing::info!("Triggering cron job: {}", job_id);
                // Record execution status
                let now = chrono::Utc::now().timestamp_millis();
                let _ = sqlx::query(
                    "UPDATE cron_jobs SET last_run_at = ?, run_count = run_count + 1 WHERE id = ?"
                )
                .bind(now)
                .bind(&job_id)
                .execute(&pool)
                .await;

                // Execute the callback task
                executor(job_def).await;
            })
        })?;

        let scheduler_uuid = self.inner.add(job).await?;
        self.job_handles.lock().insert(job_id, scheduler_uuid);
        Ok(())
    }

    /// Remove a cron job by its UUID.
    pub async fn remove_job(&self, job_id: &str) -> Result<bool> {
        let scheduler_uuid = self.job_handles.lock().remove(job_id);

        if let Some(uuid) = scheduler_uuid {
            let _ = self.inner.remove(&uuid).await;

            sqlx::query(
                "UPDATE cron_jobs SET status = 'deleted' WHERE id = ?"
            )
            .bind(job_id)
            .execute(&self.pool)
            .await?;

            tracing::info!(job_id, "Cron job removed");
            return Ok(true);
        }

        Ok(false)
    }

    /// Load all `active` jobs from SQLite and re-register them.
    async fn reload_from_db(&self) -> Result<()> {
        let jobs = sqlx::query_as::<_, CronJob>(
            "SELECT id, cron_expr, description, task_type, task_params, target_channel_id, status, created_at, last_run_at, run_count
             FROM cron_jobs WHERE status = 'active'"
        )
        .fetch_all(&self.pool)
        .await?;

        tracing::info!(count = jobs.len(), "Reloading cron jobs from database");

        for job in jobs {
            if let Err(e) = self.schedule_in_memory(job).await {
                tracing::error!("Failed to reload job: {}", e);
            }
        }

        Ok(())
    }
}
