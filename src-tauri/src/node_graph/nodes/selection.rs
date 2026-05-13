use super::*;
use crate::models::fixtures::{ChannelColour, ChannelType, FixtureDefinition, Mode};
use crate::node_graph::circle_fit;

/// Check whether a fixture mode has a given capability.
fn fixture_has_capability(def: &FixtureDefinition, mode: &Mode, capability: &str) -> bool {
    let mut has_pan = false;
    let mut has_tilt = false;
    let mut has_color = false;
    let mut has_shutter = false;

    for mode_ch in &mode.channels {
        if let Some(channel) = def.channels.iter().find(|c| c.name == mode_ch.name) {
            match channel.get_type() {
                ChannelType::Pan => has_pan = true,
                ChannelType::Tilt => has_tilt = true,
                ChannelType::Shutter => has_shutter = true,
                ChannelType::Intensity => {
                    if channel.get_colour() != ChannelColour::None {
                        has_color = true;
                    }
                }
                ChannelType::Colour => has_color = true,
                _ => {}
            }
        }
    }

    match capability {
        "movement" => has_pan || has_tilt,
        "color" => has_color,
        "strobe" => has_shutter,
        _ => false,
    }
}

/// Builds a Selection from a tag expression and spatial reference.
pub async fn build_selection_from_expression(
    ctx: &NodeExecutionContext<'_>,
    tag_expr: &str,
    spatial_reference: &str,
    rng_seed: u64,
) -> Result<Vec<Selection>, String> {
    let project_pool = ctx.project_pool;
    let resource_path_root = ctx.resource_path_root;
    let is_group_local = spatial_reference == "group_local";

    let (Some(proj_pool), Some(root)) = (project_pool, resource_path_root) else {
        return Ok(vec![Selection { items: vec![] }]);
    };

    let expr = if tag_expr.is_empty() { "all" } else { tag_expr };
    let fixtures = crate::services::groups::resolve_selection_expression_with_path(
        root,
        proj_pool,
        &ctx.graph_context.venue_id,
        expr,
        rng_seed,
    )
    .await
    .map_err(|e| format!("Selection expression failed: {}", e))?;

    // Build SelectableItems for each fixture
    // Also track group membership if group_local
    let mut fixture_items: std::collections::HashMap<String, Vec<SelectableItem>> =
        std::collections::HashMap::new();
    let mut group_items: std::collections::HashMap<String, Vec<SelectableItem>> =
        std::collections::HashMap::new();

    for fixture in &fixtures {
        // Load definition to get layout
        let def_path = root.join(&fixture.fixture_path);

        let offsets = if let Ok(def) = parse_definition(&def_path) {
            compute_head_offsets(&def, &fixture.mode_name)
        } else {
            // If missing def, assume single head at 0,0,0
            vec![crate::fixtures::layout::HeadLayout {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            }]
        };

        let mut items_for_fixture = Vec::new();

        for (i, offset) in offsets.iter().enumerate() {
            let head_id = format!("{}:{}", fixture.id, i);

            // Local offset in meters (Z-up, Y-forward data space)
            let lx = offset.x / 1000.0;
            let ly = offset.y / 1000.0;
            let lz = offset.z / 1000.0;

            // Interpret stored rotations with Y/Z swapped (legacy UI mapping).
            let rx = fixture.rot_x;
            let ry = fixture.rot_z;
            let rz = fixture.rot_y;

            // Rotate around Z (yaw)
            let (lx_z, ly_z) = (
                lx * rz.cos() as f32 - ly * rz.sin() as f32,
                lx * rz.sin() as f32 + ly * rz.cos() as f32,
            );
            let lz_z = lz;

            // Rotate around Y (pitch)
            let (lx_y, lz_y) = (
                lx_z * ry.cos() as f32 + lz_z * ry.sin() as f32,
                -lx_z * ry.sin() as f32 + lz_z * ry.cos() as f32,
            );
            let ly_y = ly_z;

            // Rotate around X (roll)
            let (ly_x, lz_x) = (
                ly_y * rx.cos() as f32 - lz_y * rx.sin() as f32,
                ly_y * rx.sin() as f32 + lz_y * rx.cos() as f32,
            );
            let lx_x = lx_y;

            let gx = fixture.pos_x as f32 + lx_x;
            let gy = fixture.pos_y as f32 + lz_x;
            let gz = fixture.pos_z as f32 + ly_x;

            items_for_fixture.push(SelectableItem {
                id: head_id,
                fixture_id: fixture.id.clone(),
                head_index: i,
                pos: (gx, gy, gz),
            });
        }

        fixture_items.insert(fixture.id.clone(), items_for_fixture);
    }

    let selections = if is_group_local {
        // Group items by their group membership
        for fixture in &fixtures {
            if let Some(items) = fixture_items.get(&fixture.id) {
                let groups =
                    crate::database::local::groups::get_groups_for_fixture(proj_pool, &fixture.id)
                        .await
                        .unwrap_or_default();

                if groups.is_empty() {
                    // Fixtures not in any group go into a placeholder group
                    group_items
                        .entry(String::new())
                        .or_default()
                        .extend(items.iter().cloned());
                } else {
                    for group in groups {
                        group_items
                            .entry(group.id)
                            .or_default()
                            .extend(items.iter().cloned());
                    }
                }
            }
        }

        // Convert to Vec<Selection>, sorted by group_id for determinism
        let mut group_ids: Vec<String> = group_items.keys().cloned().collect();
        group_ids.sort();

        group_ids
            .into_iter()
            .filter_map(|gid| group_items.remove(&gid).map(|items| Selection { items }))
            .collect()
    } else {
        // Global: single selection with all items
        let all_items: Vec<SelectableItem> = fixture_items.into_values().flatten().collect();
        vec![Selection { items: all_items }]
    };

    Ok(selections)
}

pub async fn run_node(
    node: &NodeInstance,
    ctx: &NodeExecutionContext<'_>,
    state: &mut ExecutionState,
) -> Result<bool, String> {
    let incoming_edges = ctx.incoming_edges;
    match node.type_id.as_str() {
        "get_attribute" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let selection_edge = input_edges.iter().find(|e| e.to_port == "selection");

            if let Some(edge) = selection_edge {
                if let Some(selections) = state
                    .selections
                    .get(&(edge.from_node.clone(), edge.from_port.clone()))
                {
                    let attr = node
                        .params
                        .get("attribute")
                        .and_then(|v| v.as_str())
                        .unwrap_or("index");

                    // Process each selection with its own bounds
                    let mut data = Vec::new();

                    for selection in selections {
                        let n = selection.items.len();
                        if n == 0 {
                            continue;
                        }

                        // Aggregate attributes: one scalar per selection group.
                        if attr == "count" {
                            data.push(n as f32);
                            continue;
                        }

                        // Compute bounds for this selection
                        let first = &selection.items[0];
                        let mut bounds = (
                            first.pos.0,
                            first.pos.0,
                            first.pos.1,
                            first.pos.1,
                            first.pos.2,
                            first.pos.2,
                        );
                        for item in &selection.items {
                            if item.pos.0 < bounds.0 {
                                bounds.0 = item.pos.0;
                            }
                            if item.pos.0 > bounds.1 {
                                bounds.1 = item.pos.0;
                            }
                            if item.pos.1 < bounds.2 {
                                bounds.2 = item.pos.1;
                            }
                            if item.pos.1 > bounds.3 {
                                bounds.3 = item.pos.1;
                            }
                            if item.pos.2 < bounds.4 {
                                bounds.4 = item.pos.2;
                            }
                            if item.pos.2 > bounds.5 {
                                bounds.5 = item.pos.2;
                            }
                        }
                        let (min_x, max_x, min_y, max_y, min_z, max_z) = bounds;

                        let range_x = (max_x - min_x).max(0.001); // Avoid div by zero
                        let range_y = (max_y - min_y).max(0.001);
                        let range_z = (max_z - min_z).max(0.001);

                        // Major span axis: the axis with the largest physical range
                        let major_span_axis = if range_x >= range_y && range_x >= range_z {
                            'x'
                        } else if range_y >= range_x && range_y >= range_z {
                            'y'
                        } else {
                            'z'
                        };

                        // Major count axis: the axis with the most distinct head positions
                        let mut distinct_x: std::collections::HashSet<i32> =
                            std::collections::HashSet::new();
                        let mut distinct_y: std::collections::HashSet<i32> =
                            std::collections::HashSet::new();
                        let mut distinct_z: std::collections::HashSet<i32> =
                            std::collections::HashSet::new();
                        for item in &selection.items {
                            distinct_x.insert((item.pos.0 * 1000.0).round() as i32);
                            distinct_y.insert((item.pos.1 * 1000.0).round() as i32);
                            distinct_z.insert((item.pos.2 * 1000.0).round() as i32);
                        }
                        let count_x = distinct_x.len();
                        let count_y = distinct_y.len();
                        let count_z = distinct_z.len();

                        let major_count_axis = if count_x >= count_y && count_x >= count_z {
                            'x'
                        } else if count_y >= count_x && count_y >= count_z {
                            'y'
                        } else {
                            'z'
                        };

                        // Compute circle center for circle_angle/circle_radius
                        let sum_x: f32 = selection.items.iter().map(|it| it.pos.0).sum();
                        let sum_y: f32 = selection.items.iter().map(|it| it.pos.1).sum();
                        let center_x = sum_x / n as f32;
                        let center_y = sum_y / n as f32;

                        // Compute fitted circle for angular_position (PCA + RANSAC)
                        let circle_fit_result = if attr == "angular_position" {
                            let positions: Vec<(f32, f32, f32)> =
                                selection.items.iter().map(|it| it.pos).collect();
                            circle_fit::fit_circle_3d(&positions)
                        } else {
                            None
                        };

                        // For angular_index: sort fixtures by angle from centroid, assign integer indices
                        // Uses simple atan2 (no circle fit) so it works with any arrangement including mirrored positions
                        let angular_index_map: Option<std::collections::HashMap<usize, f32>> =
                            if attr == "angular_index" {
                                let mut indexed: Vec<(usize, f32)> = selection
                                    .items
                                    .iter()
                                    .enumerate()
                                    .map(|(i, item)| {
                                        let dx = item.pos.0 - center_x;
                                        let dy = item.pos.1 - center_y;
                                        let angle = dy.atan2(dx);
                                        let normalized = ((angle + std::f32::consts::PI)
                                            / (2.0 * std::f32::consts::PI)
                                            + 0.25)
                                            % 1.0;
                                        (i, normalized)
                                    })
                                    .collect();
                                indexed.sort_by(|a, b| {
                                    a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
                                });
                                // Assign same index to fixtures at the same position (e.g. mirrored pairs)
                                let pos_epsilon = 0.5_f32;
                                let mut rank = 0_usize;
                                let mut map: std::collections::HashMap<usize, f32> =
                                    std::collections::HashMap::with_capacity(indexed.len());
                                for (k, &(orig_idx, _)) in indexed.iter().enumerate() {
                                    if k > 0 {
                                        let prev_orig = indexed[k - 1].0;
                                        let prev = &selection.items[prev_orig];
                                        let curr = &selection.items[orig_idx];
                                        let dx = curr.pos.0 - prev.pos.0;
                                        let dy = curr.pos.1 - prev.pos.1;
                                        let dz = curr.pos.2 - prev.pos.2;
                                        if (dx * dx + dy * dy + dz * dz).sqrt() > pos_epsilon {
                                            rank += 1;
                                        }
                                    }
                                    map.insert(orig_idx, rank as f32);
                                }
                                Some(map)
                            } else {
                                None
                            };

                        for (i, item) in selection.items.iter().enumerate() {
                            let val = match attr {
                                "index" => i as f32,
                                "normalized_index" => {
                                    if n > 1 {
                                        i as f32 / (n - 1) as f32
                                    } else {
                                        0.0
                                    }
                                }
                                "pos_x" => item.pos.0,
                                "pos_y" => item.pos.1,
                                "pos_z" => item.pos.2,
                                "rel_x" => (item.pos.0 - min_x) / range_x,
                                "rel_y" => (item.pos.1 - min_y) / range_y,
                                "rel_z" => (item.pos.2 - min_z) / range_z,
                                "rel_major_span" => {
                                    // Position along the axis with largest physical extent
                                    match major_span_axis {
                                        'x' => (item.pos.0 - min_x) / range_x,
                                        'y' => (item.pos.1 - min_y) / range_y,
                                        'z' => (item.pos.2 - min_z) / range_z,
                                        _ => 0.0,
                                    }
                                }
                                "rel_major_count" => {
                                    // Position along the axis with most distinct head positions
                                    match major_count_axis {
                                        'x' => (item.pos.0 - min_x) / range_x,
                                        'y' => (item.pos.1 - min_y) / range_y,
                                        'z' => (item.pos.2 - min_z) / range_z,
                                        _ => 0.0,
                                    }
                                }
                                "circle_radius" => {
                                    // Distance from center, normalized by max radius
                                    let dx = item.pos.0 - center_x;
                                    let dy = item.pos.1 - center_y;
                                    (dx * dx + dy * dy).sqrt()
                                }
                                "angular_position" => {
                                    // Angular position using fitted circle (PCA + RANSAC)
                                    // Returns 0..1 based on angle around the fitted circle
                                    if let Some(ref fit) = circle_fit_result {
                                        fit.angular_positions.get(i).copied().unwrap_or(0.0)
                                    } else {
                                        // Fallback to simple angle if fit failed
                                        let dx = item.pos.0 - center_x;
                                        let dy = item.pos.1 - center_y;
                                        let angle = dy.atan2(dx);
                                        ((angle + std::f32::consts::PI)
                                            / (2.0 * std::f32::consts::PI)
                                            + 0.25)
                                            % 1.0
                                    }
                                }
                                "angular_index" => {
                                    // Integer index sorted by angle from centroid
                                    // Fixtures sorted by atan2 angle, then assigned 0, 1, 2, ...
                                    angular_index_map
                                        .as_ref()
                                        .and_then(|map| map.get(&i).copied())
                                        .unwrap_or(i as f32)
                                }
                                _ => 0.0,
                            };
                            data.push(val);
                        }
                    }

                    let n = data.len();
                    state.signal_outputs.insert(
                        (node.id.clone(), "out".into()),
                        Signal {
                            n,
                            t: 1, // Static over time
                            c: 1, // Scalar attribute
                            data,
                        },
                    );
                }
            }
            Ok(true)
        }
        "random_select_mask" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let sel_edge = input_edges.iter().find(|e| e.to_port == "selection");
            let events_edge = input_edges.iter().find(|e| e.to_port == "events_in");
            let count_edge = input_edges.iter().find(|e| e.to_port == "count");

            let selections_opt = sel_edge.and_then(|e| {
                state
                    .selections
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });
            let events: Vec<f32> = events_edge
                .and_then(|e| {
                    state
                        .event_outputs
                        .get(&(e.from_node.clone(), e.from_port.clone()))
                })
                .cloned()
                .unwrap_or_default();

            // Default count signal is 1
            let default_count = Signal {
                n: 1,
                t: 1,
                c: 1,
                data: vec![1.0],
            };
            let count_signal = count_edge
                .and_then(|e| {
                    state
                        .signal_outputs
                        .get(&(e.from_node.clone(), e.from_port.clone()))
                })
                .unwrap_or(&default_count);

            if let Some(selections) = selections_opt {
                let avoid_repeat = node
                    .params
                    .get("avoid_repeat")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1.0)
                    > 0.5;

                let n: usize = selections.iter().map(|s| s.items.len()).sum();
                let duration =
                    (ctx.graph_context.end_time - ctx.graph_context.start_time).max(0.001);
                let t_steps = ((duration * SIMULATION_RATE).ceil() as usize).max(PREVIEW_LENGTH);

                let mut mask_data = vec![0.0; n * t_steps];

                fn hash_combine(seed: u64, v: u64) -> u64 {
                    let mut x = seed ^ v;
                    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
                    x ^ (x >> 31)
                }

                let mut node_hasher = std::collections::hash_map::DefaultHasher::new();
                std::hash::Hash::hash(&node.id, &mut node_hasher);
                let node_seed = std::hash::Hasher::finish(&node_hasher);

                let mut prev_event_idx: Option<usize> = None;
                let mut prev_selected: Vec<usize> = Vec::new();

                for t in 0..t_steps {
                    let t_sec = ctx.graph_context.start_time
                        + (t as f32 / t_steps as f32) * duration;
                    // event_idx is the count of events at or before t_sec; bumps on each event
                    // so it doubles as both "current event ordinal" and "change detector".
                    let event_idx = events.partition_point(|&p| p <= t_sec);

                    let count_t_idx = if count_signal.t <= 1 {
                        0
                    } else {
                        ((t as f32 / (t_steps - 1).max(1) as f32)
                            * (count_signal.t - 1) as f32)
                            .round() as usize
                    };
                    let count = count_signal
                        .data
                        .get(count_t_idx * count_signal.c)
                        .copied()
                        .unwrap_or(1.0)
                        .round()
                        .max(0.0) as usize;

                    if Some(event_idx) != prev_event_idx {
                        let new_selected: Vec<usize> = if event_idx == 0 || n == 0 {
                            Vec::new()
                        } else {
                            let step_seed = hash_combine(node_seed, event_idx as u64);
                            let mut scores: Vec<(usize, u64)> = (0..n)
                                .map(|i| (i, hash_combine(step_seed, i as u64)))
                                .collect();
                            scores.sort_by_key(|&(_, s)| s);

                            if avoid_repeat && !prev_selected.is_empty() {
                                let mut available: Vec<(usize, u64)> = scores
                                    .iter()
                                    .filter(|(idx, _)| !prev_selected.contains(idx))
                                    .copied()
                                    .collect();
                                if available.len() < count {
                                    let from_prev: Vec<(usize, u64)> = scores
                                        .iter()
                                        .filter(|(idx, _)| prev_selected.contains(idx))
                                        .copied()
                                        .collect();
                                    available.extend(from_prev);
                                }
                                available.into_iter().take(count).map(|(i, _)| i).collect()
                            } else {
                                scores.into_iter().take(count).map(|(i, _)| i).collect()
                            }
                        };
                        prev_selected = new_selected;
                        prev_event_idx = Some(event_idx);
                    }

                    for idx in &prev_selected {
                        let out_idx = idx * t_steps + t;
                        mask_data[out_idx] = 1.0;
                    }
                }

                state.signal_outputs.insert(
                    (node.id.clone(), "out".into()),
                    Signal {
                        n,
                        t: t_steps,
                        c: 1,
                        data: mask_data,
                    },
                );
            }
            Ok(true)
        }
        "filter_selection" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let selection_edge = input_edges.iter().find(|e| e.to_port == "selection");

            if let Some(edge) = selection_edge {
                if let Some(selections) = state
                    .selections
                    .get(&(edge.from_node.clone(), edge.from_port.clone()))
                    .cloned()
                {
                    let capability = node
                        .params
                        .get("capability")
                        .and_then(|v| v.as_str())
                        .unwrap_or("movement");

                    let (Some(proj_pool), Some(root)) = (ctx.project_pool, ctx.resource_path_root)
                    else {
                        state
                            .selections
                            .insert((node.id.clone(), "out".into()), selections);
                        return Ok(true);
                    };

                    // Cache capability check per fixture_id
                    let mut fixture_has_cap: std::collections::HashMap<String, bool> =
                        std::collections::HashMap::new();

                    // Collect unique fixture IDs
                    let unique_ids: std::collections::HashSet<String> = selections
                        .iter()
                        .flat_map(|s| s.items.iter().map(|it| it.fixture_id.clone()))
                        .collect();

                    for fixture_id in unique_ids {
                        let has = match crate::database::local::fixtures::get_fixture(
                            proj_pool,
                            &fixture_id,
                        )
                        .await
                        {
                            Ok(fixture) => {
                                let def_path = root.join(&fixture.fixture_path);
                                if let Ok(def) = parse_definition(&def_path) {
                                    if let Some(mode) =
                                        def.modes.iter().find(|m| m.name == fixture.mode_name)
                                    {
                                        fixture_has_capability(&def, mode, capability)
                                    } else {
                                        false
                                    }
                                } else {
                                    false
                                }
                            }
                            Err(_) => false,
                        };
                        fixture_has_cap.insert(fixture_id, has);
                    }

                    let filtered: Vec<Selection> = selections
                        .into_iter()
                        .map(|s| Selection {
                            items: s
                                .items
                                .into_iter()
                                .filter(|it| {
                                    fixture_has_cap
                                        .get(&it.fixture_id)
                                        .copied()
                                        .unwrap_or(false)
                                })
                                .collect(),
                        })
                        .filter(|s| !s.items.is_empty())
                        .collect();

                    state
                        .selections
                        .insert((node.id.clone(), "out".into()), filtered);
                }
            }
            Ok(true)
        }
        "mirror" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let selection_edge = input_edges.iter().find(|e| e.to_port == "selection");

            if let Some(edge) = selection_edge {
                if let Some(selections) = state
                    .selections
                    .get(&(edge.from_node.clone(), edge.from_port.clone()))
                    .cloned()
                {
                    let axis = node
                        .params
                        .get("axis")
                        .and_then(|v| v.as_str())
                        .unwrap_or("x");

                    // Compute center along chosen axis as mean of all items
                    let all_items: Vec<&SelectableItem> =
                        selections.iter().flat_map(|s| &s.items).collect();
                    let n = all_items.len();

                    if n > 0 {
                        let axis_vals: Vec<f32> = all_items
                            .iter()
                            .map(|item| match axis {
                                "y" => item.pos.1,
                                "z" => item.pos.2,
                                _ => item.pos.0, // default "x"
                            })
                            .collect();

                        let mean = axis_vals.iter().sum::<f32>() / n as f32;
                        let center = if mean.abs() < 0.1 { 0.0 } else { mean };

                        let epsilon = 0.5_f32;

                        // Build mirrored selections and side signal
                        let mut side_data = Vec::with_capacity(n);
                        let mut mirrored_selections = Vec::with_capacity(selections.len());

                        for selection in &selections {
                            let mut mirrored_items = Vec::with_capacity(selection.items.len());
                            for item in &selection.items {
                                let pos_axis = match axis {
                                    "y" => item.pos.1,
                                    "z" => item.pos.2,
                                    _ => item.pos.0,
                                };

                                let folded = (pos_axis - center).abs();
                                let new_pos = match axis {
                                    "y" => (item.pos.0, folded, item.pos.2),
                                    "z" => (item.pos.0, item.pos.1, folded),
                                    _ => (folded, item.pos.1, item.pos.2),
                                };

                                let side = if pos_axis > center + epsilon {
                                    1.0_f32
                                } else if pos_axis < center - epsilon {
                                    -1.0_f32
                                } else {
                                    0.0_f32
                                };

                                mirrored_items.push(SelectableItem {
                                    id: item.id.clone(),
                                    fixture_id: item.fixture_id.clone(),
                                    head_index: item.head_index,
                                    pos: new_pos,
                                });
                                side_data.push(side);
                            }
                            mirrored_selections.push(Selection {
                                items: mirrored_items,
                            });
                        }

                        state
                            .selections
                            .insert((node.id.clone(), "out".into()), mirrored_selections);
                        state.signal_outputs.insert(
                            (node.id.clone(), "side".into()),
                            Signal {
                                n,
                                t: 1,
                                c: 1,
                                data: side_data,
                            },
                        );
                    }
                }
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub fn get_node_types() -> Vec<NodeTypeDef> {
    vec![
        NodeTypeDef {
            id: "get_attribute".into(),
            name: "Get Attribute".into(),
            description: Some("Extracts spatial attributes from a selection into a Signal.".into()),
            category: Some("Selection".into()),
            inputs: vec![PortDef {
                id: "selection".into(),
                name: "Selection".into(),
                port_type: PortType::Selection,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "attribute".into(),
                name: "Attribute".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some("index".into()), // index, normalized_index, count, pos_x/y/z, rel_x/y/z, rel_major_span/count, circle_radius, angular_position, angular_index
            }],
        },
        NodeTypeDef {
            id: "random_select_mask".into(),
            name: "Random Select Mask".into(),
            description: Some(
                "Re-rolls a random subset of N items on every incoming event.".into(),
            ),
            category: Some("Selection".into()),
            inputs: vec![
                PortDef {
                    id: "selection".into(),
                    name: "Selection".into(),
                    port_type: PortType::Selection,
                },
                PortDef {
                    id: "events_in".into(),
                    name: "Events".into(),
                    port_type: PortType::Events,
                },
                PortDef {
                    id: "count".into(),
                    name: "Count".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Mask".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "avoid_repeat".into(),
                name: "Avoid Repeat".into(),
                param_type: ParamType::Number, // 0 or 1
                default_number: Some(1.0),
                default_text: None,
            }],
        },
        NodeTypeDef {
            id: "filter_selection".into(),
            name: "Filter Selection".into(),
            description: Some(
                "Filters a selection to only include fixtures with a specific capability.".into(),
            ),
            category: Some("Selection".into()),
            inputs: vec![PortDef {
                id: "selection".into(),
                name: "Selection".into(),
                port_type: PortType::Selection,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Selection".into(),
                port_type: PortType::Selection,
            }],
            params: vec![ParamDef {
                id: "capability".into(),
                name: "Capability".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some("movement".into()), // movement, color, strobe
            }],
        },
        NodeTypeDef {
            id: "mirror".into(),
            name: "Mirror".into(),
            description: Some(
                "Folds fixture positions across a mirror axis for symmetric spatial effects."
                    .into(),
            ),
            category: Some("Selection".into()),
            inputs: vec![PortDef {
                id: "selection".into(),
                name: "Selection".into(),
                port_type: PortType::Selection,
            }],
            outputs: vec![
                PortDef {
                    id: "out".into(),
                    name: "Selection".into(),
                    port_type: PortType::Selection,
                },
                PortDef {
                    id: "side".into(),
                    name: "Side".into(),
                    port_type: PortType::Signal,
                },
            ],
            params: vec![ParamDef {
                id: "axis".into(),
                name: "Axis".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some("x".into()),
            }],
        },
    ]
}
