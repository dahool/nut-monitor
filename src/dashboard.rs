use std::sync::Arc;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
};
use askama::Template;
use crate::AppState;
use crate::metrics::{fetch_ups_metrics, status_to_message};

#[derive(Template)]
#[template(path = "template.html")]
pub struct DashboardTemplate {
    pub ups_name: String,
    pub ups_host: String,
    pub mfr_model: String,
    pub ups_status: String,
    pub status_class: String,
    pub battery_charge: String,
    pub charge_pct: String,
    pub ups_load: String,
    pub load_pct: String,
    pub ups_load_watt: String,
    pub ups_runtime: String,
    pub input_voltage: String,
    pub input_voltage_nominal: String,
    pub battery_voltage_nominal: String,
    pub battery_voltage: String,
}

pub async fn html_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let m = fetch_ups_metrics(&state);

    let status_class = match status_to_message(m.status.as_str(), m.status.clone()).as_str() {
        "Online (AC)" => "status-online",
        "Online (Charging)" => "status-online",
        "On Battery" => "status-battery",
        "Low Battery ⚠️" => "status-critical",
        _ => "status-unknown",
    };

    let template = DashboardTemplate {
        ups_name: state.ups_name.clone(),
        ups_host: state.ups_host.clone(),
        mfr_model: m.model.clone(),
        ups_status: m.status.clone(),
        status_class: status_class.to_string(),
        battery_charge: m.battery_charge.clone(),
        charge_pct: if m.battery_charge == "N/A" { "0".to_string() } else { m.battery_charge.clone() },
        ups_load: m.ups_load.clone(),
        load_pct: if m.ups_load == "N/A" { "0".to_string() } else { m.ups_load.clone() },
        ups_load_watt: m.ups_load_watt.clone(),
        ups_runtime: m.runtime_formatted.clone(),
        input_voltage: m.input_voltage.clone(),
        input_voltage_nominal: m.input_voltage_nominal.clone(),
        battery_voltage_nominal: m.battery_voltage_nominal.clone(),
        battery_voltage: m.battery_voltage.clone(),
    };

    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Template render error: {}", e)).into_response(),
    }
}
