use axum::extract::State;

use crate::state::AppState;

pub async fn metrics_handler(State(state): State<AppState>) -> String {
    state.metrics.render()
}
