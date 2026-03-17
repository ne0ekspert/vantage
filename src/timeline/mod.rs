use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};

use crate::domain::{Event, Feature, Workspace};

pub fn advance_playback(workspace: &mut Workspace, frame_delta: StdDuration) {
    if !workspace.app_state.timeline.playing {
        return;
    }

    let advance_millis =
        (frame_delta.as_secs_f32() * workspace.app_state.timeline.playback_speed * 1000.0) as i64;
    let next_time =
        workspace.app_state.timeline.current_time + Duration::milliseconds(advance_millis);

    if next_time >= workspace.app_state.timeline.range_end {
        workspace.app_state.timeline.current_time = workspace.app_state.timeline.range_start;
    } else {
        workspace.app_state.timeline.current_time = next_time;
    }
}

pub fn time_to_fraction(time: DateTime<Utc>, start: DateTime<Utc>, end: DateTime<Utc>) -> f32 {
    let total = (end - start).num_milliseconds().max(1) as f32;
    let elapsed = (time - start)
        .num_milliseconds()
        .clamp(0, (end - start).num_milliseconds()) as f32;
    elapsed / total
}

pub fn scrub_fraction_to_time(
    fraction: f32,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> DateTime<Utc> {
    let clamped = fraction.clamp(0.0, 1.0);
    let total = (end - start).num_milliseconds() as f32;
    start + Duration::milliseconds((total * clamped) as i64)
}

pub fn feature_is_active(feature: &Feature, current_time: DateTime<Utc>) -> bool {
    temporal_bounds_active(feature.time_start, feature.time_end, current_time)
}

pub fn event_is_active(event: &Event, current_time: DateTime<Utc>) -> bool {
    temporal_bounds_active(Some(event.start_time), event.end_time, current_time)
}

fn temporal_bounds_active(
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    current_time: DateTime<Utc>,
) -> bool {
    match (start, end) {
        (None, None) => true,
        (Some(start), None) => current_time >= start,
        (None, Some(end)) => current_time <= end,
        (Some(start), Some(end)) => current_time >= start && current_time <= end,
    }
}
