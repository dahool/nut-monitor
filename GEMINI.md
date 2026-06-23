# Project Instruction Context: nut-monitor-web

## Project Overview
A lightweight web dashboard and alerting monitor for UPS (Uninterruptible Power Supply) devices, written in **Rust** using the **Axum** web framework. It interfaces with Network UPS Tools (NUT) via the local/remote `upsc` utility to retrieve real-time power metrics, renders a responsive dark-themed HTML dashboard, and broadcasts emergency alerts to mobile devices via **Firebase Cloud Messaging (FCM) v1 API**.

### Main Technologies & Libraries
- **Language/Edition:** Rust (2021 edition)
- **Web Framework:** Axum 0.7 + Tokio 1.0 (Async Runtime)
- **Database:** SQLite (embedded via `rusqlite` 0.31 with `bundled` feature). It stores registered mobile/client device tokens in a local `devices.db` file.
- **Frontend Dashboard:** HTML/CSS with modular, modern styling, compiled type-safely at build time using the **Askama** template engine (templates are located in the `/templates` directory).
- **Notifications:** Google FCM v1 API with JSON Web Tokens (JWT) for OAuth2 authentication (using `jsonwebtoken` with RS256 algorithm and `reqwest` with rustls).
- **Logging/Tracing:** Structured logging via `tracing`, `tracing-subscriber`, and `tower-http` trace middleware.

### Architecture and Flow
1. **Background Monitor Loop:** A Tokio background thread (interval configured by `MONITOR_INTERVAL_SECS`, default 10s) periodically queries the UPS using the system `upsc` command.
2. **Threshold Evaluation:** Current metrics are evaluated against alert thresholds:
   - Status changes (e.g., Online -> On Battery, On Battery -> Low Battery)
   - Battery charge falling below 50%
   - UPS load exceeding 80%
   - Backup battery runtime dropping below 15 minutes (900 seconds)
3. **Alerting Broadcast:** When an alert is triggered, the background loop reads registered device tokens from the SQLite database and broadcasts push notifications via FCM.
4. **Web API & Dashboard:**
   - **Dashboard (GET `/`):** Renders a styled, responsive dark-themed dashboard presenting live UPS load, charge, voltage, runtime, status, and device metadata.
   - **Metrics JSON (GET `/api/status`):** Outputs raw UPS metrics.
   - **Device Management:**
     - `POST /api/register`: Registers/updates device details in SQLite.
     - `GET /api/devices`: Lists all registered devices.
     - `DELETE /api/devices/:id`: Removes a device by ID.
     - `POST /api/test-fcm`: Triggers manual push notification testing.

---

## Environment Variables and Configuration
Configure the application using the following environment variables:

| Variable | Default | Description |
|---|---|---|
| `UPS_NAME` | `ups` | Name of the UPS device configured in NUT (e.g., `eaton`, `apc`). |
| `UPS_HOST` | `localhost` | Hostname or IP of the machine hosting the `upsd` server. |
| `MONITOR_INTERVAL_SECS` | `10` | How frequently the background task queries the UPS and checks thresholds. |
| `FCM_PROJECT_ID` | *Optional* | Google Cloud Project ID for Firebase Messaging. |
| `FCM_CLIENT_EMAIL` | *Optional* | Google Service Account Client Email for JWT auth. |
| `FCM_PRIVATE_KEY` | *Optional* | Google Service Account Private Key (supports literal `\n` formatting). |

*Note: If any `FCM_*` credentials are empty, background alerting and manual test endpoints will skip broadcasting and log a warning on startup.*

---

## Building and Running

### Prerequisites
- **Rust Toolchain:** Stable release.
- **NUT Client (`upsc`):** The application relies on executing `upsc` to get metrics. The executing environment must have `nut-client` installed.

### Local Development Commands

- **Verify Compilation:**
  ```bash
  cargo check
  ```
- **Run Locally (Development Mode):**
  Provide environment variables for target UPS or FCM testing as needed:
  ```bash
  UPS_NAME=ups UPS_HOST=192.168.1.100 cargo run
  ```
- **Build Release Binary:**
  ```bash
  cargo build --release
  ```
  The compiled output will be generated at `target/release/nut-monitor-web`.

- **Run Tests:**
  ```bash
  cargo test
  ```
  *Note: There are currently no unit tests defined in the codebase.*

### Docker Deployment
The project is optimized for containerization using multi-stage builds.

- **Build Docker Image Locally:**
  ```bash
  docker build -t nut-monitor-web:latest .
  ```
- **Run via Docker Compose:**
  Review and customize settings inside `docker-compose.yml`:
  ```bash
  docker compose up -d
  ```

---

## Development Conventions

### Code Style & Structure
- **Modular backend architecture:** The application is split into separate logical sub-modules:
  - `src/metrics.rs`: Parsers for `upsc` outputs and calculation of real power consumption in Watts.
  - `src/alerts.rs`: Alert threshold evaluation loop and Google FCM client.
  - `src/dashboard.rs`: Askama template definitions and HTML view handler.
  - `src/web.rs`: JSON API routers, handlers for devices registration, test operations, etc.
  - `src/main.rs`: Application entrypoint that parses environment settings, initializes database schemas, spawns background services, and boots the Axum server.
- **Database Connectivity:** A single SQLite connection is managed via a `Mutex` inside `AppState` and shared safely using an `Arc<AppState>`.
- **Axum State Management:** All router handlers access shared states by extracting `State(state): State<Arc<AppState>>`.
- **Ecosystem Libraries:** Avoid using native TLS to maintain fast, reproducible multi-architecture builds. Keep `reqwest` with `default-features = false` and `rustls-tls` enabled.

### Testing Guidelines
- When introducing new backend features or refactoring alert criteria, add unit or integration tests to verify functionality.
- Tests can be co-located at the bottom of files within a `mod tests` block.
- Mocking is recommended for `Command::new("upsc")` processes or database queries during automated testing to avoid environmental dependencies.

### CI/CD Workflow
- A GitHub Actions workflow (`.github/workflows/build.yml`) automatically builds, tags, and publishes multi-platform (`linux/amd64`, `linux/arm64`) Docker images to **GitHub Container Registry (GHCR)** on every push to the `master` branch or when a release semver tag (e.g., `v0.2.0`) is added.
