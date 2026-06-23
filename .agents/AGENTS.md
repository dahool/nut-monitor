# Codebase Rules & Guidelines for Agents

Welcome to `nut-monitor-web`! These guidelines are project-specific conventions that you MUST follow when modifying or extending this codebase.

## Code Style & Architectural Constraints

### 1. Modular Code Architecture
Do not return to a single-file architecture. The backend is structured into separate modules:
*   [main.rs](file:///workspace/projects/nut-monitor/src/main.rs): Entry point, DB schema initialization, server booting, and background task orchestration.
*   [metrics.rs](file:///workspace/projects/nut-monitor/src/metrics.rs): Metrics structs, `upsc` output parsing, and calculation logic.
*   [alerts.rs](file:///workspace/projects/nut-monitor/src/alerts.rs): Threshold evaluators, FCM notifications, and SQLite token lists.
*   [dashboard.rs](file:///workspace/projects/nut-monitor/src/dashboard.rs): Askama templates and HTML handlers.
*   [web.rs](file:///workspace/projects/nut-monitor/src/web.rs): Axum routing, API endpoints for devices, status query, and manually triggered push-notification test endpoints.

### 2. Frontend & Template Compilation (Askama)
*   The dashboard HTML is dynamically compiled at build time using the **Askama** template engine.
*   Templates are located in the [templates/](file:///workspace/projects/nut-monitor/templates/) root-level directory.
*   Avoid adding raw HTML placeholders. Always use typed fields in `DashboardTemplate` inside [dashboard.rs](file:///workspace/projects/nut-monitor/src/dashboard.rs) and Askama template syntax (`{{ field }}`) inside `template.html`.

### 3. Containerization Requirements
*   If you introduce new build-time dependencies or folders, always update the [Dockerfile](file:///workspace/projects/nut-monitor/Dockerfile) builder stage so that the compilation in container environments succeeds.
*   Because Askama templates compile at build-time, ensure the `templates/` directory is copied inside the Dockerfile compilation workspace.

### 4. Dependency Constraints
*   Do not enable native TLS for `reqwest` or other HTTP clients to maintain fast, simple cross-architecture compiling. Keep `default-features = false` and enable `rustls-tls` to fetch updates/broadcast alerts safely.

### 5. Database Conventions & Uniqueness
*   All devices registered in the SQLite database must be unique. The database schema enforces a `PRIMARY KEY` on `device_id` and a `UNIQUE` constraint on the `device_token` column.
*   Always use `INSERT OR REPLACE` when saving new token keys to prevent duplicate keys across devices or conflicting entries.

### 6. Test-Driven Development
*   Always decouple your calculation logic from process or database executions (e.g. extracting helper functions like `parse_upsc_output`) to enable automated unit tests with mocks and test fixtures.
*   Ensure that any new changes maintain or expand unit test coverage at the bottom of [main.rs](file:///workspace/projects/nut-monitor/src/main.rs#L125).
