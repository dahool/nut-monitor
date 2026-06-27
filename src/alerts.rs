use tracing::{info, error};
use crate::AppState;
use crate::metrics::{fetch_ups_metrics, status_to_message};

pub fn get_registered_tokens(state: &AppState) -> Vec<String> {
    let db = state.db_conn.lock().unwrap();
    let mut stmt = db.prepare("SELECT device_token FROM devices").unwrap();
    let token_iter = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
    token_iter.filter_map(|t| t.ok()).collect::<Vec<String>>()
}

pub async fn evaluate_alerts(state: &AppState) {
    let m = fetch_ups_metrics(state);
    if m.status == "Disconnected" || m.status.contains("Error") { return; }

    let mut trigger = false;
    let mut title = String::new();
    let mut message = String::new();

    {
        let mut alerts = state.last_alerts.lock().unwrap();

        if !alerts.last_status.is_empty() && alerts.last_status != m.status {
            trigger = true;
            let msg_status = status_to_message(m.status.as_str(), m.status.clone());
            title = format!("UPS Status Changed: {}", msg_status);
            message = format!("Device shifted from {} to {}.", alerts.last_status, msg_status);
        }
        alerts.last_status = m.status.clone();

        if let Ok(charge) = m.battery_charge.parse::<u32>() {
            if charge < 50 {
                if !alerts.battery_low_sent {
                    trigger = true;
                    title = "Alert: Low Battery Capacity".to_string();
                    message = format!("UPS battery charge dropped under threshold: {}%", charge);
                    alerts.battery_low_sent = true;
                }
            } else { alerts.battery_low_sent = false; }
        }

        if let Ok(load) = m.ups_load.parse::<u32>() {
            if load > 80 {
                if !alerts.load_high_sent {
                    trigger = true;
                    title = "Alert: Critical Overload".to_string();
                    message = format!("UPS load capacity consumption is exceeding metrics: {}%", load);
                    alerts.load_high_sent = true;
                }
            } else { alerts.load_high_sent = false; }
        }

        // Modificado: Solo evalúa el runtime si está en batería ("OB" u "OB DISCHRG") o batería baja ("LB")
        let is_on_battery = m.status.starts_with("OB") || m.status.starts_with("LB");

        if is_on_battery {
            if let Ok(seconds) = m.runtime_seconds.parse::<u32>() {
                if seconds < 900 {
                    if !alerts.runtime_low_sent {
                        trigger = true;
                        title = "Alert: Critical Low Runtime".to_string();
                        message = format!("UPS backup window is under 15 minutes ({} min remaining).", seconds / 60);
                        alerts.runtime_low_sent = true;
                    }
                } else { alerts.runtime_low_sent = false; }
            }
        } else {
            // Si vuelve a la red eléctrica (AC), reseteamos el flag para que pueda volver a alertar en el futuro
            alerts.runtime_low_sent = false;
        }
    }

    if trigger {
        if let Some(ref config) = state.fcm_config {
            let tokens = get_registered_tokens(state);
            info!("System threshold triggered. Broadcasting alert notifications to {} devices...", tokens.len());
            for token in tokens {
                let _ = send_fcm_v1_notification(config, &token, &title, &message).await;
            }
        }
    }
}

pub async fn send_fcm_v1_notification(
    config: &crate::FcmConfig,
    device_token: &str,
    title: &str,
    body: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let truncated_token = if device_token.len() > 12 { &device_token[..12] } else { device_token };
    info!("Dispatching FCM notification to token starting with: {}... Title: \"{}\"", truncated_token, title);

    let client = reqwest::Client::new();
    let now = chrono::Utc::now().timestamp();
    
    let claims = serde_json::json!({
        "iss": config.client_email,
        "scope": "https://www.googleapis.com/auth/firebase.messaging",
        "aud": "https://oauth2.googleapis.com/token",
        "exp": now + 3600,
        "iat": now
    });

    let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(config.private_key.as_bytes())?;
    let jwt = jsonwebtoken::encode(&header, &claims, &key)?;

    let token_res: serde_json::Value = client.post("https://oauth2.googleapis.com/token")
        .form(&[("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"), ("assertion", &jwt)])
        .send().await?.json().await?;

    let access_token = token_res["access_token"].as_str().ok_or_else(|| {
        error!("FCM authentication missing token property in Google response structural schema");
        "Failed parsing access token"
    })?;
    
    let url = format!("https://fcm.googleapis.com/v1/projects/{}/messages:send", config.project_id);
    
    let payload = serde_json::json!({
        "message": {
            "token": device_token,
            "notification": { "title": title, "body": body },
            "android": { "priority": "high", "notification": { "sound": "default", "channel_id": "ups_alerts" } }
        }
    });

    let res = client.post(&url).bearer_auth(access_token).json(&payload).send().await?;
    
    if res.status().is_success() {
        info!("Successfully delivered FCM notification payload to device token instance.");
        Ok(())
    } else {
        let err_text = res.text().await.unwrap_or_default();
        error!("FCM transmission rejection from Google Gateway API service: {}", err_text);
        Err("FCM service delivery failure".into())
    }
}
