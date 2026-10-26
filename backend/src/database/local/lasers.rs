//! A venue's laser zones and its projectors' limits (`laser_zones`,
//! `laser_limits`; `docs/design/lasers.md`).

use uuid::Uuid;

use crate::database::local::deletes;
use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, Write};
use crate::models::lasers::{LaserLimit, LaserZone};

/// The venue's zones, by name.
pub async fn list_zones(access: &mut impl AuthorizedVenue) -> Result<Vec<LaserZone>, String> {
    let venue_id = access.venue_id().to_owned();
    let rows: Vec<(String, String, f64, f64, f64, f64)> = sqlx::query_as(
        "SELECT id, name, turn_min, turn_max, tilt_min, tilt_max FROM laser_zones
         WHERE venue_id = ? ORDER BY name COLLATE NOCASE, created_at, id",
    )
    .bind(&venue_id)
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|error| format!("Failed to list laser zones: {error}"))?;
    Ok(rows
        .into_iter()
        .map(
            |(id, name, turn_min, turn_max, tilt_min, tilt_max)| LaserZone {
                id,
                venue_id: venue_id.clone(),
                name,
                turn: [turn_min, turn_max],
                tilt: [tilt_min, tilt_max],
            },
        )
        .collect())
}

/// Create a zone (`id` `None`) or replace one of this venue's. Answers the
/// zone as stored.
pub async fn save_zone(
    access: &mut VenueAccess<'_, Write>,
    id: Option<&str>,
    name: &str,
    turn: [f64; 2],
    tilt: [f64; 2],
) -> Result<Option<LaserZone>, String> {
    let zone = LaserZone {
        id: id.map_or_else(|| Uuid::new_v4().to_string(), str::to_owned),
        venue_id: access.venue_id().to_owned(),
        name: name.to_owned(),
        turn,
        tilt,
    };
    let uid = access.principal().map(str::to_owned);
    let query = if id.is_some() {
        "UPDATE laser_zones SET name = ?, turn_min = ?, turn_max = ?, tilt_min = ?, tilt_max = ?
         WHERE id = ? AND venue_id = ?"
    } else {
        "INSERT INTO laser_zones (name, turn_min, turn_max, tilt_min, tilt_max, id, venue_id, uid)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
    };
    let mut statement = sqlx::query(query)
        .bind(&zone.name)
        .bind(zone.turn[0])
        .bind(zone.turn[1])
        .bind(zone.tilt[0])
        .bind(zone.tilt[1])
        .bind(&zone.id)
        .bind(&zone.venue_id);
    if id.is_none() {
        statement = statement.bind(uid);
    }
    let done = statement
        .execute(&mut *access.connection())
        .await
        .map_err(|error| format!("Failed to save laser zone: {error}"))?;
    Ok((done.rows_affected() == 1).then_some(zone))
}

/// Answers how many zones it deleted: one, or none when `id` is not this
/// venue's. Laser clips naming it draw in the venue's first zone.
pub async fn delete_zone(access: &mut VenueAccess<'_, Write>, id: &str) -> Result<u64, String> {
    let venue_id = access.venue_id().to_owned();
    deletes::delete_where(
        access.connection(),
        "laser_zones",
        "id = ? AND venue_id = ?",
        &[id, &venue_id],
    )
    .await
    .map(|deleted| deleted as u64)
}

/// Every projector limit in the venue.
pub async fn list_limits(access: &mut impl AuthorizedVenue) -> Result<Vec<LaserLimit>, String> {
    let venue_id = access.venue_id().to_owned();
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT id, points FROM laser_limits WHERE venue_id = ? ORDER BY id")
            .bind(&venue_id)
            .fetch_all(&mut *access.connection())
            .await
            .map_err(|error| format!("Failed to list laser limits: {error}"))?;
    Ok(rows
        .into_iter()
        .filter_map(|(fixture_id, points)| match serde_json::from_str(&points) {
            Ok(points) => Some(LaserLimit { fixture_id, points }),
            Err(error) => {
                log::warn!("laser limit for {fixture_id} is not a polygon: {error}");
                None
            }
        })
        .collect())
}

/// Set a projector's limit, or clear it with `None`. Answers whether the
/// fixture is this venue's.
pub async fn set_limit(
    access: &mut VenueAccess<'_, Write>,
    fixture_id: &str,
    points: Option<&[[f64; 2]]>,
) -> Result<bool, String> {
    let venue_id = access.venue_id().to_owned();
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM fixtures WHERE id = ? AND venue_id = ?)")
            .bind(fixture_id)
            .bind(&venue_id)
            .fetch_one(&mut *access.connection())
            .await
            .map_err(|error| format!("Failed to read the fixture: {error}"))?;
    if !exists {
        return Ok(false);
    }
    let Some(points) = points else {
        deletes::delete_where(
            access.connection(),
            "laser_limits",
            "id = ? AND venue_id = ?",
            &[fixture_id, &venue_id],
        )
        .await?;
        return Ok(true);
    };
    let json = serde_json::to_string(points).map_err(|error| error.to_string())?;
    let uid = access.principal().map(str::to_owned);
    sqlx::query(
        "INSERT INTO laser_limits (id, uid, venue_id, points) VALUES (?, ?, ?, ?)
         ON CONFLICT (id) DO UPDATE SET points = excluded.points",
    )
    .bind(fixture_id)
    .bind(uid)
    .bind(&venue_id)
    .bind(json)
    .execute(&mut *access.connection())
    .await
    .map_err(|error| format!("Failed to save laser limit: {error}"))?;
    Ok(true)
}

/// `20261025130000_laser_elements.sql` against the old laser nodes and the
/// Color clips that coloured them, as a real score held them, and
/// `20261026120000_laser_figures.sql` against the laser nodes it left.
#[cfg(test)]
mod migration_tests {
    use luma_patterns::clip_graph::ClipGraph;
    use serde_json::{json, Value};
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::SqlitePool;
    use std::borrow::Cow;

    const LASER_ELEMENTS: i64 = 20_261_025_130_000;
    const LASER_FIGURES: i64 = 20_261_026_120_000;
    const PROJECTOR: &str = "Generic/Generic-ILDA-Laser-Projector.qxf";

    /// A venue of two projectors and a par, migrated up to `until`.
    async fn database(directory: &tempfile::TempDir, until: i64) -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(directory.path().join("luma.db"))
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .unwrap();
        let mut before = sqlx::migrate!("./migrations");
        before.migrations = Cow::Owned(
            before
                .migrations
                .iter()
                .filter(|migration| migration.version < until)
                .cloned()
                .collect(),
        );
        before.run(&pool).await.unwrap();
        for statement in [
            "INSERT INTO venues (id, name) VALUES ('v', 'Club')",
            "INSERT INTO scores (id, track_id, venue_id) VALUES ('s', 't', 'v')",
            "INSERT INTO fixture_groups (id, venue_id, name) VALUES ('g1', 'v', 'lasers')",
            "INSERT INTO fixture_groups (id, venue_id, name) VALUES ('g2', 'v', 'everything')",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }
        for (id, path) in [
            ("p1", PROJECTOR),
            ("p2", PROJECTOR),
            ("par", "Generic/Par.qxf"),
        ] {
            sqlx::query(
                "INSERT INTO fixtures (id, venue_id, address, num_channels, manufacturer, model,
                 mode_name, fixture_path) VALUES (?, 'v', 1, 1, 'm', 'm', 'm', ?)",
            )
            .bind(id)
            .bind(path)
            .execute(&pool)
            .await
            .unwrap();
            let groups: &[&str] = if id == "par" { &["g2"] } else { &["g1", "g2"] };
            for group in groups {
                sqlx::query(
                    "INSERT INTO fixture_group_members (id, fixture_id, group_id) VALUES (?, ?, ?)",
                )
                .bind(format!("{group}-{id}"))
                .bind(id)
                .bind(group)
                .execute(&pool)
                .await
                .unwrap();
            }
        }
        pool
    }

    async fn clip(pool: &SqlitePool, id: &str, start: f64, selection: &str, nodes: Value) {
        let graph = json!({"version": 3, "nodes": nodes}).to_string();
        sqlx::query(
            "INSERT INTO clips (id, uid, score_id, name, graph_json, graph, start, duration, seed,
             selection_json, blend_mode) VALUES (?, 'u', 's', ?, ?, '', ?, 4, '0', ?, 'replace')",
        )
        .bind(id)
        .bind(id)
        .bind(graph)
        .bind(start)
        .bind(json!({"expression": selection}).to_string())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn graph(pool: &SqlitePool, id: &str) -> Option<ClipGraph> {
        let json: Option<String> = sqlx::query_scalar("SELECT graph_json FROM clips WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await
            .unwrap();
        json.map(|json| {
            let graph = ClipGraph::from_json(&json).unwrap_or_else(|e| panic!("{id}: {e:?}"));
            graph
                .check()
                .unwrap_or_else(|e| panic!("{id}: {}\n{json}", e.0));
            graph
        })
    }

    /// The laser's first figure.
    fn laser(graph: &ClipGraph) -> &luma_patterns::clip_graph::Node {
        let laser = graph.output().expect("an output").1;
        let figure = laser.inputs["figures"].sources().next().expect("a figure");
        &graph.nodes[figure]
    }

    /// Each element's x on one projector of `count` elements.
    fn xs(graph: &ClipGraph, count: usize) -> Vec<f64> {
        let cells: Vec<luma_patterns::Cell> = (0..count)
            .map(|i| luma_patterns::Cell {
                id: format!("p1:{i}"),
                group: "lasers".into(),
                world: [i as f64 * 0.01, 0., 1.],
                uvz: [i as f64 * 0.01, 0., 1.],
            })
            .collect();
        let frame = luma_patterns::Frame {
            cells: &cells,
            features: None,
            beat: 1.,
            clip_start: 0.,
            clip_duration: 4.,
            seed: 0,
        };
        let prepared =
            luma_patterns::PreparedGraph::new(&luma_patterns::standard_library(), graph, frame)
                .unwrap();
        let out = prepared.evaluate_batch(&[1.]).unwrap();
        let mut ids: Vec<String> = cells.iter().map(|c| c.id.clone()).collect();
        ids.sort_by_key(|id| id[3..].parse::<usize>().unwrap());
        luma_patterns::laser::Element::read(&out[&luma_patterns::laser::element_output(0)], &ids, 1)
            .unwrap()[0]
            .iter()
            .map(|(_, samples)| samples[0].at[0])
            .collect()
    }

    #[tokio::test]
    async fn old_laser_nodes_and_their_color_clips_move_to_elements() {
        let directory = tempfile::tempdir().unwrap();
        let pool = database(&directory, LASER_ELEMENTS).await;
        let old = |inputs: Value, shape: &str| json!({"laser1": {"kind": "laser", "settings": {"zone": "", "shape": shape}, "inputs": inputs}});
        clip(
            &pool,
            "fan",
            0.,
            "lasers",
            json!({
                "mirror1": {"kind": "mirror", "inputs": {"direction": [1, 0, 0]}},
                "laser1": {"kind": "laser", "settings": {"zone": "", "shape": "fan"},
                           "inputs": {"size": 1, "beams": 16, "heads": {"node": "mirror1"}}},
            }),
        )
        .await;
        clip(
            &pool,
            "offset",
            10.,
            "lasers",
            old(json!({"x": -0.55, "size": 0.35, "beams": 5}), "fan"),
        )
        .await;
        clip(&pool, "sweep", 20., "lasers", json!({
            "time1": {"kind": "time", "inputs": {"every": 2}},
            "curve1": {"kind": "curve", "settings": {"kind": "number"},
                       "inputs": {"x": {"node": "time1"}, "low": -0.5, "high": 0.5}},
            "curve2": {"kind": "curve", "settings": {"kind": "number"},
                       "inputs": {"x": {"node": "time1"}, "low": 0.1, "high": 0.3}},
            "laser1": {"kind": "laser", "settings": {"zone": "", "shape": "fan"},
                       "inputs": {"x": {"node": "curve1"}, "size": {"node": "curve2"}, "beams": 4}},
        }))
        .await;
        clip(
            &pool,
            "sheet",
            30.,
            "lasers",
            old(
                json!({"x": 0.25, "join": 1, "size": 0.1, "beams": 16}),
                "fan",
            ),
        )
        .await;
        clip(
            &pool,
            "ring",
            40.,
            "lasers",
            old(json!({"join": 1, "size": 0.6, "beams": 24}), "circle"),
        )
        .await;
        clip(
            &pool,
            "circle",
            50.,
            "lasers",
            old(json!({"size": 0.5, "beams": 6}), "circle"),
        )
        .await;
        clip(
            &pool,
            "grid",
            60.,
            "lasers",
            old(json!({"size": 0.8, "beams": 16}), "grid"),
        )
        .await;
        // A wired join goes, and what only it read.
        clip(&pool, "pulse", 110., "lasers", json!({
            "time1": {"kind": "time"},
            "curve1": {"kind": "curve", "settings": {"kind": "number"}, "inputs": {"x": {"node": "time1"}}},
            "curve2": {"kind": "curve", "settings": {"kind": "number"}, "inputs": {"x": {"node": "time1"}}},
            "laser1": {"kind": "laser", "settings": {"zone": "", "shape": "fan"},
                       "inputs": {"join": {"node": "curve1"}, "size": {"node": "curve2"}, "beams": 8}},
        }))
        .await;
        // A flat colour with a brightness folds into a constant colour.
        clip(
            &pool,
            "flat",
            70.,
            "lasers",
            old(json!({"beams": 3}), "fan"),
        )
        .await;
        clip(&pool, "flat color", 70., "lasers", json!({
            "color1": {"kind": "color", "inputs": {"color": [0.2, 0.4, 0.6], "brightness": 0.5}},
        }))
        .await;
        // A shuffled stab's graph is copied in.
        clip(
            &pool,
            "stab",
            80.,
            "lasers",
            old(json!({"beams": 16}), "fan"),
        )
        .await;
        clip(&pool, "stab color", 80., "lasers", json!({
            "time1": {"kind": "time", "inputs": {"every": 0.5}},
            "shuffle1": {"kind": "shuffle", "inputs": {"time": {"node": "time1"}}},
            "space1": {"kind": "space", "settings": {"kind": "order", "wrap": "no"},
                       "inputs": {"heads": {"node": "shuffle1"}}},
            "curve1": {"kind": "curve", "settings": {"kind": "number"},
                       "inputs": {"x": {"node": "space1"}, "shape": {"points": [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]}}},
            "color1": {"kind": "color", "inputs": {"color": [1, 0, 0], "brightness": {"node": "curve1"}}},
        }))
        .await;
        // A Color clip over more than projectors stays, folded or not.
        clip(
            &pool,
            "wide",
            90.,
            "everything",
            old(json!({"beams": 2}), "fan"),
        )
        .await;
        clip(&pool, "wide color", 90., "everything", json!({
            "time1": {"kind": "time"},
            "curve1": {"kind": "curve", "settings": {"kind": "color"},
                       "inputs": {"x": {"node": "time1"}, "gradient": {"stops": [{"t": 0, "color": [0, 0, 1]}]}}},
            "color1": {"kind": "color", "inputs": {"color": {"node": "curve1"}}},
        }))
        .await;
        // Two Color clips over one laser clip pair with neither.
        clip(
            &pool,
            "roll",
            100.,
            "lasers",
            old(json!({"beams": 2}), "fan"),
        )
        .await;
        clip(
            &pool,
            "roll color",
            100.,
            "lasers",
            json!({"color1": {"kind": "color"}}),
        )
        .await;
        clip(
            &pool,
            "roll color 2",
            100.,
            "lasers",
            json!({"color1": {"kind": "color"}}),
        )
        .await;
        sqlx::query(
            "INSERT INTO drafts (id, uid, score_id, base_json, state_json) VALUES ('d', 'u', 's', ?, '{}')",
        )
        .bind(json!({"clips": {"fan": {"graph": old(json!({}), "fan")}}}).to_string())
        .execute(&pool)
        .await
        .unwrap();

        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        let fan = graph(&pool, "fan").await.unwrap();
        let node = laser(&fan);
        assert_eq!(node.setting("kind"), Some("beams"));
        assert_eq!(node.inputs["count"], 16.0.into());
        assert!(
            !node.inputs.contains_key("x"),
            "the home layout is the old fan"
        );
        assert_eq!(
            node.inputs["heads"],
            luma_patterns::clip_graph::Input::wire("mirror1")
        );

        // The old fan's beams, end to end across `size` at x.
        let offset = graph(&pool, "offset").await.unwrap();
        let got = xs(&offset, 5);
        for (i, x) in got.iter().enumerate() {
            let want = -0.55 + 0.35 * (i as f64 / 2. - 1.);
            assert!((x - want).abs() < 1e-9, "{got:?}");
        }
        let sweep = graph(&pool, "sweep").await.unwrap();
        let got = xs(&sweep, 4);
        assert!(got.windows(2).all(|pair| pair[0] < pair[1]), "{got:?}");

        for (id, kind) in [("sheet", "line"), ("ring", "ring"), ("grid", "grid")] {
            let graph = graph(&pool, id).await.unwrap();
            let node = laser(&graph);
            assert_eq!(node.setting("kind"), Some(kind), "{id}");
            assert!(!node.inputs.contains_key("join") && !node.inputs.contains_key("beams"));
        }
        assert_eq!(
            graph(&pool, "grid").await.unwrap().laser().unwrap().count(),
            16
        );
        assert_eq!(
            graph(&pool, "sheet")
                .await
                .unwrap()
                .laser()
                .unwrap()
                .count(),
            1
        );
        // A circle of beams goes round the circle.
        let circle = graph(&pool, "circle").await.unwrap();
        let got = xs(&circle, 6);
        assert!(
            got.iter().any(|x| *x > 0.4) && got.iter().any(|x| *x < -0.4),
            "{got:?}"
        );

        let flat = graph(&pool, "flat").await.unwrap();
        let luma_patterns::clip_graph::Input::Color(rgb) = &laser(&flat).inputs["color"] else {
            panic!("a flat colour: {:?}", laser(&flat).inputs);
        };
        for (got, want) in rgb.iter().zip([0.1, 0.2, 0.3]) {
            assert!((got - want).abs() < 1e-9, "{rgb:?}");
        }
        let stab = graph(&pool, "stab").await.unwrap();
        assert!(stab.nodes.contains_key("c_shuffle1") && stab.nodes.contains_key("c_mix"));
        assert!(graph(&pool, "flat color").await.is_none());
        assert!(graph(&pool, "stab color").await.is_none());
        // Over more than projectors: folded and kept.
        let wide = graph(&pool, "wide").await.unwrap();
        assert_eq!(
            laser(&wide).inputs["color"],
            luma_patterns::clip_graph::Input::wire("c_curve1")
        );
        assert!(graph(&pool, "wide color").await.is_some());
        let roll = graph(&pool, "roll").await.unwrap();
        assert!(!laser(&roll).inputs.contains_key("color"));
        assert!(graph(&pool, "roll color").await.is_some());

        let pulse = graph(&pool, "pulse").await.unwrap();
        assert_eq!(laser(&pulse).setting("kind"), Some("beams"));
        assert!(!pulse.nodes.contains_key("curve1") && pulse.nodes.contains_key("curve2"));

        let drafts: i64 = sqlx::query_scalar("SELECT count(*) FROM drafts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(drafts, 0);
    }

    /// A laser node splits into a figure, which takes what it draws, and a
    /// laser that draws it, in clips and in drafts.
    #[tokio::test]
    async fn laser_nodes_split_into_a_figure_and_the_laser_that_draws_it() {
        use luma_patterns::clip_graph::{Input, Kind};
        let directory = tempfile::tempdir().unwrap();
        let pool = database(&directory, LASER_FIGURES).await;
        let fan = json!({
            "mirror1": {"kind": "mirror", "inputs": {"direction": [1, 0, 0]}},
            "time1": {"kind": "time"},
            "fade": {"kind": "curve", "settings": {"kind": "number"}, "inputs": {"x": {"node": "time1"}}},
            "laser1": {"kind": "laser", "settings": {"kind": "beams", "zone": "z1"},
                       "inputs": {"heads": {"node": "mirror1"}, "count": 6, "size": 0.7,
                                  "color": [1, 0, 0], "alpha": {"node": "fade"}}},
        });
        clip(&pool, "fan", 0., "lasers", fan.clone()).await;
        // A node already named figure1 keeps its name.
        clip(
            &pool,
            "taken",
            10.,
            "lasers",
            json!({
                "figure1": {"kind": "time"},
                "spin": {"kind": "curve", "settings": {"kind": "number"},
                         "inputs": {"x": {"node": "figure1"}, "low": -90, "high": 90}},
                "laser1": {"kind": "laser", "settings": {"kind": "ring", "zone": ""},
                           "inputs": {"rotation": {"node": "spin"}, "alpha": 0.5}},
            }),
        )
        .await;
        clip(
            &pool,
            "wash",
            20.,
            "lasers",
            json!({"color1": {"kind": "color"}}),
        )
        .await;
        let draft = json!({"clips": {
            "c-1": {"seed": 13369261119795889533_u64, "graph": {"version": 3, "nodes": fan}},
            "c-2": {"graph": {"version": 3, "nodes": {"laser1": {"kind": "laser"}}}},
        }})
        .to_string();
        sqlx::query(
            "INSERT INTO drafts (id, uid, score_id, base_json, state_json) VALUES ('d', 'u', 's', ?, ?)",
        )
        .bind(&draft)
        .bind(&draft)
        .execute(&pool)
        .await
        .unwrap();
        let wash_before: String =
            sqlx::query_scalar("SELECT graph_json FROM clips WHERE id = 'wash'")
                .fetch_one(&pool)
                .await
                .unwrap();

        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        let fan = graph(&pool, "fan").await.unwrap();
        let laser1 = &fan.nodes["laser1"];
        assert_eq!(laser1.setting("zone"), Some("z1"));
        assert_eq!(
            laser1.inputs["figures"],
            Input::List(vec![Input::wire("figure1")])
        );
        assert_eq!(laser1.inputs["alpha"], Input::wire("fade"));
        assert_eq!(laser1.inputs.len(), 2);
        let figure = &fan.nodes["figure1"];
        assert_eq!(figure.kind, Kind::Figure);
        assert_eq!(figure.setting("kind"), Some("beams"));
        assert_eq!(figure.inputs["heads"], Input::wire("mirror1"));
        assert_eq!(figure.inputs["count"], 6.0.into());
        assert_eq!(figure.inputs["size"], 0.7.into());
        assert_eq!(figure.inputs["color"], Input::Color([1., 0., 0.]));
        assert!(!figure.inputs.contains_key("alpha"));
        assert_eq!(fan.laser().unwrap().count(), 6);

        let taken = graph(&pool, "taken").await.unwrap();
        assert_eq!(taken.nodes["figure1"].kind, Kind::Time);
        let figure = &taken.nodes["laser1_figure"];
        assert_eq!(figure.setting("kind"), Some("ring"));
        assert_eq!(figure.inputs["rotation"], Input::wire("spin"));
        assert_eq!(taken.nodes["laser1"].inputs["alpha"], 0.5.into());

        let wash_after: String =
            sqlx::query_scalar("SELECT graph_json FROM clips WHERE id = 'wash'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            wash_after, wash_before,
            "a graph with no laser is untouched"
        );

        let (base, state): (String, String) =
            sqlx::query_as("SELECT base_json, state_json FROM drafts WHERE id = 'd'")
                .fetch_one(&pool)
                .await
                .unwrap();
        for text in [base, state] {
            assert!(text.contains("13369261119795889533"), "{text}");
            let clips: Value = serde_json::from_str::<Value>(&text).unwrap()["clips"].clone();
            for id in ["c-1", "c-2"] {
                let graph: ClipGraph = serde_json::from_value(clips[id]["graph"].clone()).unwrap();
                graph.check().unwrap_or_else(|e| panic!("{id}: {}", e.0));
                assert_eq!(graph.nodes["figure1"].kind, Kind::Figure, "{id}");
            }
        }
    }
}
