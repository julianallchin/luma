//! Tag auto-generation and management service.

use sqlx::SqlitePool;

use crate::database::local::fixtures as fixtures_db;
use crate::database::local::tags as tags_db;
use crate::models::fixtures::PatchedFixture;

/// Venue bounds for normalization
#[derive(Debug, Clone)]
pub struct VenueBounds {
    pub min_x: f64,
    pub max_x: f64,
    pub min_y: f64,
    pub max_y: f64,
    pub min_z: f64,
    pub max_z: f64,
}

impl VenueBounds {
    /// Compute bounds from a list of fixtures
    pub fn from_fixtures(fixtures: &[PatchedFixture]) -> Self {
        if fixtures.is_empty() {
            return VenueBounds {
                min_x: 0.0,
                max_x: 0.0,
                min_y: 0.0,
                max_y: 0.0,
                min_z: 0.0,
                max_z: 0.0,
            };
        }

        let mut min_x = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        let mut min_z = f64::INFINITY;
        let mut max_z = f64::NEG_INFINITY;

        for f in fixtures {
            min_x = min_x.min(f.pos_x);
            max_x = max_x.max(f.pos_x);
            min_y = min_y.min(f.pos_y);
            max_y = max_y.max(f.pos_y);
            min_z = min_z.min(f.pos_z);
            max_z = max_z.max(f.pos_z);
        }

        VenueBounds {
            min_x,
            max_x,
            min_y,
            max_y,
            min_z,
            max_z,
        }
    }
}

/// Normalize a value to -1..1 range
fn normalize_axis(value: f64, min: f64, max: f64) -> f64 {
    let span = max - min;
    if span.abs() <= f64::EPSILON {
        0.0
    } else {
        ((value - min) / span) * 2.0 - 1.0
    }
}

/// Compute which spatial tags apply to a fixture based on its position
pub fn compute_spatial_tags_for_fixture(
    fixture: &PatchedFixture,
    bounds: &VenueBounds,
) -> Vec<String> {
    let mut tags = Vec::new();

    // Normalize position to -1..1 range
    let norm_x = normalize_axis(fixture.pos_x, bounds.min_x, bounds.max_x);
    let norm_y = normalize_axis(fixture.pos_y, bounds.min_y, bounds.max_y);
    let norm_z = normalize_axis(fixture.pos_z, bounds.min_z, bounds.max_z);

    // Spatial thresholds
    const SIDE_THRESHOLD: f64 = 0.2;
    const CENTER_THRESHOLD: f64 = 0.3;

    // Left/Right (X axis - stage left is negative, stage right is positive)
    if norm_x < -SIDE_THRESHOLD {
        tags.push("left".to_string());
    } else if norm_x > SIDE_THRESHOLD {
        tags.push("right".to_string());
    }

    // Front/Back (Y axis - front/downstage is negative, back/upstage is positive)
    if norm_y < -SIDE_THRESHOLD {
        tags.push("front".to_string());
    } else if norm_y > SIDE_THRESHOLD {
        tags.push("back".to_string());
    }

    // High/Low (Z axis - high is positive, low is negative)
    if norm_z > SIDE_THRESHOLD {
        tags.push("high".to_string());
    } else if norm_z < -SIDE_THRESHOLD {
        tags.push("low".to_string());
    }

    // Center (all axes near center)
    if norm_x.abs() < CENTER_THRESHOLD
        && norm_y.abs() < CENTER_THRESHOLD
        && norm_z.abs() < CENTER_THRESHOLD
    {
        tags.push("center".to_string());
    }

    tags
}

/// Detect if a set of fixtures forms a circular arrangement
pub fn is_circular_arrangement(fixtures: &[PatchedFixture]) -> bool {
    if fixtures.len() < 3 {
        return false;
    }

    // Calculate centroid
    let (sum_x, sum_y) = fixtures
        .iter()
        .fold((0.0, 0.0), |acc, f| (acc.0 + f.pos_x, acc.1 + f.pos_y));
    let count = fixtures.len() as f64;
    let center_x = sum_x / count;
    let center_y = sum_y / count;

    // Calculate radii from center
    let mut radii = Vec::with_capacity(fixtures.len());
    for f in fixtures {
        let dx = f.pos_x - center_x;
        let dy = f.pos_y - center_y;
        radii.push((dx * dx + dy * dy).sqrt());
    }

    // Check if radii are consistent (low coefficient of variation)
    let mean = radii.iter().sum::<f64>() / count;
    if mean < 0.05 {
        return false; // Too small, probably not a circle
    }
    let variance = radii.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / count;
    let std_dev = variance.sqrt();

    // Circular if std_dev/mean < 0.2
    (std_dev / mean) < 0.2
}

/// Recompute all spatial tag assignments for a venue
pub async fn regenerate_spatial_tags(pool: &SqlitePool, venue_id: i64) -> Result<(), String> {
    // 1. Get all fixtures with positions
    let fixtures = fixtures_db::get_patched_fixtures(pool, venue_id).await?;

    if fixtures.is_empty() {
        return Ok(());
    }

    // 2. Compute venue bounds
    let bounds = VenueBounds::from_fixtures(&fixtures);

    // 3. Ensure spatial tags exist
    let spatial_tags = tags_db::ensure_spatial_tags_exist(pool, venue_id).await?;

    // 4. Clear existing auto-generated tag assignments
    tags_db::clear_auto_generated_tag_assignments(pool, venue_id).await?;

    // 5. Get the 'all' tag
    let all_tag = tags_db::get_tag_by_name(pool, venue_id, "all").await?;

    // 6. Assign spatial tags based on positions
    for fixture in &fixtures {
        let tag_names = compute_spatial_tags_for_fixture(fixture, &bounds);

        for tag_name in &tag_names {
            if let Some(tag) = spatial_tags.iter().find(|t| &t.name == tag_name) {
                tags_db::assign_tag_to_fixture(pool, &fixture.id, tag.id).await?;
            }
        }

        // Always assign 'all' tag
        if let Some(tag) = &all_tag {
            tags_db::assign_tag_to_fixture(pool, &fixture.id, tag.id).await?;
        }
    }

    // 7. Check for circular arrangement and assign 'circular' tag if detected
    if is_circular_arrangement(&fixtures) {
        if let Some(circular_tag) = spatial_tags.iter().find(|t| t.name == "circular") {
            for fixture in &fixtures {
                tags_db::assign_tag_to_fixture(pool, &fixture.id, circular_tag.id).await?;
            }
        }
    }

    Ok(())
}

/// Initialize tags for a new venue (called when venue is created or first accessed)
pub async fn initialize_venue_tags(pool: &SqlitePool, venue_id: i64) -> Result<(), String> {
    // Check if tags already exist
    let existing_tags = tags_db::list_tags(pool, venue_id).await?;
    if !existing_tags.is_empty() {
        return Ok(());
    }

    // Create spatial tags
    tags_db::ensure_spatial_tags_exist(pool, venue_id).await?;

    // Regenerate assignments based on fixture positions
    regenerate_spatial_tags(pool, venue_id).await?;

    Ok(())
}
