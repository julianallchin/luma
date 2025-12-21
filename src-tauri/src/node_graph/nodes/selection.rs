use super::*;

pub async fn run_node(
    node: &NodeInstance,
    ctx: &NodeExecutionContext<'_>,
    state: &mut ExecutionState,
) -> Result<bool, String> {
    let incoming_edges = ctx.incoming_edges;
    let project_pool = ctx.project_pool;
    let resource_path_root = ctx.resource_path_root;
    match node.type_id.as_str() {

            "select" => {
                // 1. Parse selected IDs
                let ids_json = node
                    .params
                    .get("selected_ids")
                    .and_then(|v| v.as_str())
                    .unwrap_or("[]");
                let selected_ids: Vec<String> = serde_json::from_str(ids_json).unwrap_or_default();

                if let Some(proj_pool) = project_pool {
                    let fixtures = crate::database::local::fixtures::get_all_fixtures(proj_pool)
                        .await
                        .map_err(|e| format!("Select node failed to fetch fixtures: {}", e))?;

                    let mut selected_items = Vec::new();

                    // Pre-process selection set for O(1) lookup
                    // We need to handle "FixtureID" (all heads) vs "FixtureID:HeadIdx" (specific head)

                    for fixture in fixtures {
                        // 2. Load definition to get layout
                        let def_path = if let Some(root) = &resource_path_root {
                            root.join(&fixture.fixture_path)
                        } else {
                            PathBuf::from(&fixture.fixture_path)
                        };

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

                        // Check if this fixture is involved in selection
                        let fixture_selected = selected_ids.contains(&fixture.id);

                        for (i, offset) in offsets.iter().enumerate() {
                            let head_id = format!("{}:{}", fixture.id, i);
                            let head_selected = selected_ids.contains(&head_id);

                            // Include if whole fixture selected OR specific head selected
                            if fixture_selected || head_selected {
                                // Apply rotation (Euler ZYX convention typically)
                                // Local offset in mm
                                let lx = offset.x / 1000.0;
                                let ly = offset.y / 1000.0;
                                let lz = offset.z / 1000.0;

                                let rx = fixture.rot_x;
                                let ry = fixture.rot_y;
                                let rz = fixture.rot_z;

                                // Rotate around X
                                // y' = y*cos(rx) - z*sin(rx)
                                // z' = y*sin(rx) + z*cos(rx)
                                let (ly_x, lz_x) = (
                                    ly * rx.cos() as f32 - lz * rx.sin() as f32,
                                    ly * rx.sin() as f32 + lz * rx.cos() as f32,
                                );
                                let lx_x = lx;

                                // Rotate around Y
                                // x'' = x'*cos(ry) + z'*sin(ry)
                                // z'' = -x'*sin(ry) + z'*cos(ry)
                                let (lx_y, lz_y) = (
                                    lx_x * ry.cos() as f32 + lz_x * ry.sin() as f32,
                                    -lx_x * ry.sin() as f32 + lz_x * ry.cos() as f32,
                                );
                                let ly_y = ly_x;

                                // Rotate around Z
                                // x''' = x''*cos(rz) - y''*sin(rz)
                                // y''' = x''*sin(rz) + y''*cos(rz)
                                let (lx_z, ly_z) = (
                                    lx_y * rz.cos() as f32 - ly_y * rz.sin() as f32,
                                    lx_y * rz.sin() as f32 + ly_y * rz.cos() as f32,
                                );
                                let lz_z = lz_y;

                                let gx = fixture.pos_x as f32 + lx_z;
                                let gy = fixture.pos_y as f32 + ly_z;
                                let gz = fixture.pos_z as f32 + lz_z;

                                selected_items.push(SelectableItem {
                                    id: head_id,
                                    fixture_id: fixture.id.clone(),
                                    head_index: i,
                                    pos: (gx, gy, gz),
                                });
                            }
                        }
                    }

                    state.selections.insert(
                        (node.id.clone(), "out".into()),
                        Selection {
                            items: selected_items,
                        },
                    );
                }
                Ok(true)
            }
            "get_attribute" => {
                let input_edges = incoming_edges
                    .get(node.id.as_str())
                    .cloned()
                    .unwrap_or_default();
                let selection_edge = input_edges.iter().find(|e| e.to_port == "selection");

                if let Some(edge) = selection_edge {
                    if let Some(selection) =
                        state.selections.get(&(edge.from_node.clone(), edge.from_port.clone()))
                    {
                        let attr = node
                            .params
                            .get("attribute")
                            .and_then(|v| v.as_str())
                            .unwrap_or("index");

                        let n = selection.items.len();
                        let mut data = Vec::with_capacity(n);

                        // Compute bounds for normalization if needed
                        let (min_x, max_x, min_y, max_y, min_z, max_z) = if n > 0 {
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
                            bounds
                        } else {
                            (0.0, 1.0, 0.0, 1.0, 0.0, 1.0)
                        };

                        let range_x = (max_x - min_x).max(0.001); // Avoid div by zero
                        let range_y = (max_y - min_y).max(0.001);
                        let range_z = (max_z - min_z).max(0.001);

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
                                _ => 0.0,
                            };
                            data.push(val);
                        }

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
                let trig_edge = input_edges.iter().find(|e| e.to_port == "trigger");

                let selection_opt = sel_edge
                    .and_then(|e| state.selections.get(&(e.from_node.clone(), e.from_port.clone())));
                let trigger_opt = trig_edge
                    .and_then(|e| state.signal_outputs.get(&(e.from_node.clone(), e.from_port.clone())));

                if let (Some(selection), Some(trigger)) = (selection_opt, trigger_opt) {
                    let count = node
                        .params
                        .get("count")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(1.0) as usize;
                    let avoid_repeat = node
                        .params
                        .get("avoid_repeat")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(1.0)
                        > 0.5;

                    let n = selection.items.len();
                    let t_steps = trigger.t;

                    let mut mask_data = vec![0.0; n * t_steps];

                    // Helper for hashing
                    fn hash_combine(seed: u64, v: u64) -> u64 {
                        let mut x = seed ^ v;
                        x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                        x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
                        x ^ (x >> 31)
                    }

                    // Node ID hash
                    let mut node_hasher = std::collections::hash_map::DefaultHasher::new();
                    std::hash::Hash::hash(&node.id, &mut node_hasher);
                    let node_seed = std::hash::Hasher::finish(&node_hasher);

                    // Track previous selection for avoid_repeat
                    let mut prev_selected: Vec<usize> = Vec::new();
                    let mut prev_trig_seed: Option<i64> = None;
                    // Use system time for true randomness across pattern executions
                    let time_seed = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0);
                    // Counter for additional randomness on each trigger change within this execution
                    let mut selection_counter: u64 = 0;

                    for t in 0..t_steps {
                        // Get trigger value at this time step.
                        // Broadcast Trigger N: use index 0 since it's likely a control signal.
                        let trig_val = trigger.data.get(t * trigger.c).copied().unwrap_or(0.0);

                        // Seed combines: node_id + time + trigger_value + counter
                        let trig_seed = (trig_val * 1000.0) as i64; // Sensitivity 0.001
                        let step_seed = hash_combine(
                            hash_combine(hash_combine(node_seed, time_seed), trig_seed as u64),
                            selection_counter,
                        );

                        // Check if trigger changed (new selection event)
                        let trigger_changed = prev_trig_seed.is_none_or(|prev| prev != trig_seed);

                        // Generate scores for each item
                        let mut scores: Vec<(usize, u64)> = (0..n)
                            .map(|i| {
                                let item_seed = hash_combine(step_seed, i as u64);
                                (i, item_seed)
                            })
                            .collect();

                        // Sort by score (random shuffle)
                        scores.sort_by_key(|&(_, s)| s);

                        // Determine selection based on trigger state
                        let selected: Vec<usize> = if !trigger_changed && !prev_selected.is_empty()
                        {
                            // Trigger unchanged - reuse previous selection
                            prev_selected.clone()
                        } else if avoid_repeat && trigger_changed && !prev_selected.is_empty() {
                            // Trigger changed with avoid_repeat - filter out previous selection
                            let mut available: Vec<(usize, u64)> = scores
                                .iter()
                                .filter(|(idx, _)| !prev_selected.contains(idx))
                                .copied()
                                .collect();

                            // If not enough available, add back from prev_selected by score
                            if available.len() < count {
                                let mut from_prev: Vec<(usize, u64)> = scores
                                    .iter()
                                    .filter(|(idx, _)| prev_selected.contains(idx))
                                    .copied()
                                    .collect();
                                available.append(&mut from_prev);
                            }

                            let new_selected: Vec<usize> = available
                                .into_iter()
                                .take(count)
                                .map(|(idx, _)| idx)
                                .collect();
                            prev_selected = new_selected.clone();
                            prev_trig_seed = Some(trig_seed);
                            selection_counter += 1;
                            new_selected
                        } else {
                            // First selection or avoid_repeat disabled
                            let new_selected: Vec<usize> =
                                scores.into_iter().take(count).map(|(idx, _)| idx).collect();
                            prev_selected = new_selected.clone();
                            prev_trig_seed = Some(trig_seed);
                            selection_counter += 1;
                            new_selected
                        };

                        // Set 1.0 for selected items
                        for idx in &selected {
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
        _ => Ok(false),
    }
}

pub fn get_node_types() -> Vec<NodeTypeDef> {
    vec![
        NodeTypeDef {
            id: "select".into(),
            name: "Select".into(),
            description: Some("Selects specific fixtures or primitives.".into()),
            category: Some("Selection".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Selection".into(),
                port_type: PortType::Selection,
            }],
            params: vec![ParamDef {
                id: "selected_ids".into(),
                name: "Selected IDs".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some("[]".into()), // JSON array of strings
            }],
        },
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
                default_text: Some("index".into()), // index, normalized_index, pos_x, pos_y, pos_z, rel_x, rel_y, rel_z
            }],
        },
        NodeTypeDef {
            id: "random_select_mask".into(),
            name: "Random Select Mask".into(),
            description: Some("Randomly selects N items based on a trigger signal.".into()),
            category: Some("Selection".into()),
            inputs: vec![
                PortDef {
                    id: "selection".into(),
                    name: "Selection".into(),
                    port_type: PortType::Selection,
                },
                PortDef {
                    id: "trigger".into(),
                    name: "Trigger".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Mask".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "count".into(),
                    name: "Count".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "avoid_repeat".into(),
                    name: "Avoid Repeat".into(),
                    param_type: ParamType::Number, // 0 or 1
                    default_number: Some(1.0),
                    default_text: None,
                },
            ],
        },
    ]
}
