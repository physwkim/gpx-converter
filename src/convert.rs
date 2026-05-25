//! GPX -> TCX Course conversion.
//!
//! The output format was reverse-engineered from a TCX Course file produced by a
//! map app. The core rules: accumulate distance (Haversine) and synthesize
//! timestamps assuming a constant 20 km/h. Map-app-specific non-standard
//! attributes (sectionIndex, etc.) are not emitted — a standard TCX subset.

use chrono::{SecondsFormat, TimeDelta, Utc};
use roxmltree::{Document, Node};

/// Speed used by the example; drives distance/time synthesis (= 20 km/h).
/// Verified: point5 distance 382.62 m / 5.5556 = 68.9 s, start 22:04:07 + 68 s = 22:05:15 (matches example).
const SPEED_MPS: f64 = 5.5556;

/// Max course-name length (chars). Some devices truncate long names, so cap it up front.
const MAX_NAME_CHARS: usize = 50;

struct Pt {
    lat: f64,
    lon: f64,
    ele: Option<f64>,
}

/// Conversion result: the download filename stem (without extension) and the TCX XML body.
pub struct Tcx {
    pub filename: String,
    pub xml: String,
}

/// Converts GPX bytes into TCX Course XML.
///
/// `upload_name` is the original uploaded filename (used for the name fallback and download stem).
pub fn gpx_to_tcx(bytes: &[u8], upload_name: &str) -> Result<Tcx, String> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| "The file is not valid UTF-8 text.".to_string())?;
    let doc = Document::parse(text).map_err(|e| format!("failed to parse the file as XML: {e}"))?;
    let root = doc.root_element();

    // Extract points: tracks -> routes -> waypoints, in fallback order. Lenient by
    // design — only lat/lon/ele are read; any non-standard sibling elements that
    // real-world exports add (kakaomap's <description>, <extensions>/<kakaomap-meta>,
    // etc.) are ignored rather than rejected.
    let mut pts = collect_points(root, "trkpt");
    if pts.is_empty() {
        pts = collect_points(root, "rtept");
    }
    if pts.is_empty() {
        pts = collect_points(root, "wpt");
    }
    if pts.is_empty() {
        return Err("The GPX has no coordinate points.".to_string());
    }

    // Name: metadata/name -> first trk/name -> filename stem -> "Course".
    // Scoped lookups so a <wpt>/<rtept> <name> (a POI label) is never mistaken
    // for the course name.
    let stem = file_stem(upload_name);
    let meta_name = root
        .children()
        .find(|c| c.is_element() && c.tag_name().name() == "metadata")
        .and_then(|m| child_text(m, "name"));
    let trk_name = root
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "trk")
        .and_then(|t| child_text(t, "name"));
    let name = meta_name
        .or(trk_name)
        .or_else(|| (!stem.is_empty()).then(|| stem.clone()))
        .unwrap_or_else(|| "Course".to_string());

    // Cumulative distance (Haversine). The first point is 0.
    let mut cum = vec![0.0_f64; pts.len()];
    for i in 1..pts.len() {
        let d = haversine(pts[i - 1].lat, pts[i - 1].lon, pts[i].lat, pts[i].lon);
        cum[i] = cum[i - 1] + d;
    }
    let total = *cum.last().unwrap();

    // Per-point elapsed whole seconds, synthesized at SPEED_MPS (floored) but forced
    // strictly increasing. GPX points closer than ~5.6 m, or exact duplicates (stops),
    // would otherwise floor to the same second and emit duplicate <Time> values; the
    // `.max(prev + 1)` makes every Trackpoint time distinct by construction. Sparse
    // routes are unaffected. Every emitted time — Trackpoints, the Start CoursePoint,
    // and the Lap total — derives from this single array.
    let mut secs = vec![0_i64; pts.len()];
    for i in 1..pts.len() {
        let ideal = (cum[i] / SPEED_MPS) as i64;
        secs[i] = ideal.max(secs[i - 1] + 1);
    }
    let total_secs = *secs.last().unwrap();

    // Start time is the current UTC at conversion. Absolute time is irrelevant for course following, so it's fine.
    let start = Utc::now();
    let at = |elapsed: i64| -> String {
        (start + TimeDelta::seconds(elapsed)).to_rfc3339_opts(SecondsFormat::Secs, true)
    };

    let first = &pts[0];
    let last = pts.last().unwrap();

    let mut s = String::with_capacity(pts.len() * 220 + 1024);
    s.push_str("<?xml version=\"1.0\" encoding=\"utf-8\" standalone=\"no\"?>\n");
    s.push_str(
        "<TrainingCenterDatabase xmlns=\"http://www.garmin.com/xmlschemas/TrainingCenterDatabase/v2\" \
xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
xsi:schemaLocation=\"http://www.garmin.com/xmlschemas/TrainingCenterDatabase/v2 \
http://www.garmin.com/xmlschemas/TrainingCenterDatabasev2.xsd\">\n",
    );
    s.push_str("<Folders />\n<Courses>\n<Course>\n");
    s.push_str(&format!(
        "<Name>{}</Name>\n",
        xml_escape(&clamp_name(&name))
    ));

    // Lap: total time/distance and begin/end positions.
    s.push_str("<Lap>\n");
    s.push_str(&format!(
        "<TotalTimeSeconds>{total_secs}</TotalTimeSeconds>\n"
    ));
    s.push_str(&format!("<DistanceMeters>{total:.2}</DistanceMeters>\n"));
    s.push_str(&format!(
        "<BeginPosition><LatitudeDegrees>{:.6}</LatitudeDegrees><LongitudeDegrees>{:.6}</LongitudeDegrees></BeginPosition>\n",
        first.lat, first.lon
    ));
    s.push_str(&format!(
        "<EndPosition><LatitudeDegrees>{:.6}</LatitudeDegrees><LongitudeDegrees>{:.6}</LongitudeDegrees></EndPosition>\n",
        last.lat, last.lon
    ));
    s.push_str("<Intensity>Active</Intensity>\n</Lap>\n");

    // Track: one Trackpoint per point.
    s.push_str("<Track>\n");
    for (i, p) in pts.iter().enumerate() {
        s.push_str("<Trackpoint>");
        s.push_str(&format!("<Time>{}</Time>", at(secs[i])));
        s.push_str(&format!(
            "<Position><LatitudeDegrees>{:.6}</LatitudeDegrees><LongitudeDegrees>{:.6}</LongitudeDegrees></Position>",
            p.lat, p.lon
        ));
        if let Some(e) = p.ele {
            s.push_str(&format!("<AltitudeMeters>{e:.2}</AltitudeMeters>"));
        }
        s.push_str(&format!("<DistanceMeters>{:.2}</DistanceMeters>", cum[i]));
        s.push_str("</Trackpoint>\n");
    }
    s.push_str("</Track>\n");

    // CoursePoint: a single Start point (Generic). Minimal, matching the example.
    s.push_str("<CoursePoint><Name>Start</Name>");
    s.push_str(&format!("<Time>{}</Time>", at(secs[0])));
    s.push_str(&format!(
        "<Position><LatitudeDegrees>{:.6}</LatitudeDegrees><LongitudeDegrees>{:.6}</LongitudeDegrees></Position>",
        first.lat, first.lon
    ));
    if let Some(e) = first.ele {
        s.push_str(&format!("<AltitudeMeters>{e:.2}</AltitudeMeters>"));
    }
    s.push_str("<PointType>Generic</PointType><Notes></Notes></CoursePoint>\n");

    s.push_str("</Course>\n</Courses>\n</TrainingCenterDatabase>\n");

    let filename = if stem.is_empty() {
        "course".to_string()
    } else {
        stem
    };
    Ok(Tcx { filename, xml: s })
}

/// Collects points with the given tag name (`trkpt`/`rtept`/`wpt`) anywhere under
/// `root`. Namespace prefixes are ignored (matched by local name). A point needs
/// numeric `lat`/`lon` attributes; `<ele>` is optional. Any other child element is
/// ignored, so non-standard `<extensions>` and the like never break extraction.
fn collect_points(root: Node, tag: &str) -> Vec<Pt> {
    root.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == tag)
        .filter_map(|n| {
            let lat = n.attribute("lat")?.trim().parse::<f64>().ok()?;
            let lon = n.attribute("lon")?.trim().parse::<f64>().ok()?;
            let ele = n
                .children()
                .find(|c| c.is_element() && c.tag_name().name() == "ele")
                .and_then(|c| c.text())
                .and_then(|t| t.trim().parse::<f64>().ok());
            Some(Pt { lat, lon, ele })
        })
        .collect()
}

/// Trimmed text of `parent`'s first direct child element with the given local
/// name, or `None` if absent or blank.
fn child_text(parent: Node, tag: &str) -> Option<String> {
    parent
        .children()
        .find(|c| c.is_element() && c.tag_name().name() == tag)
        .and_then(|c| c.text())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Great-circle distance (m) between two lat/lon coordinates. No external crate.
fn haversine(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R: f64 = 6_371_000.0; // Earth mean radius (m)
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    R * c
}

/// XML text escaping (for element content / names).
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Filename stem with path separators removed and the last extension stripped.
fn file_stem(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match base.rsplit_once('.') {
        Some((stem, _ext)) if !stem.is_empty() => stem.to_string(),
        _ => base.to_string(),
    }
}

/// Trims surrounding whitespace, then truncates by char count (UTF-8 boundary safe).
fn clamp_name(s: &str) -> String {
    s.trim().chars().take(MAX_NAME_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic route along a meridian (same longitude); latitude increases by
    // 0.01° each step. No personal data. Along a meridian the Haversine distance
    // equals R*Δlat exactly, so distances are deterministic:
    // 0.01° = 6_371_000 * 0.01 * π/180 ≈ 1111.95 m, 4 segments → ≈ 4447.80 m.
    const SYNTHETIC_GPX: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<gpx version="1.1" creator="test" xmlns="http://www.topografix.com/GPX/1/1">
<metadata><name>Test Route</name></metadata>
<trk><name>trk</name><trkseg>
<trkpt lat="35.000000" lon="129.000000"><ele>10.0</ele></trkpt>
<trkpt lat="35.010000" lon="129.000000"><ele>12.0</ele></trkpt>
<trkpt lat="35.020000" lon="129.000000"><ele>11.0</ele></trkpt>
<trkpt lat="35.030000" lon="129.000000"><ele>13.0</ele></trkpt>
<trkpt lat="35.040000" lon="129.000000"><ele>9.0</ele></trkpt>
</trkseg></trk>
</gpx>"#;

    const SEGMENT_M: f64 = 6_371_000.0 * 0.01 * std::f64::consts::PI / 180.0; // ≈ 1111.95

    fn count_occurrences(hay: &str, needle: &str) -> usize {
        hay.matches(needle).count()
    }

    #[test]
    fn converts_track_to_course() {
        let tcx = gpx_to_tcx(SYNTHETIC_GPX, "route.gpx").expect("conversion succeeds");

        // 5 trkpt -> 5 Trackpoints; clean form without non-standard attributes.
        assert_eq!(
            count_occurrences(&tcx.xml, "<Trackpoint>"),
            5,
            "trackpoint count"
        );
        assert!(
            tcx.xml.contains("<Trackpoint><Time>"),
            "Trackpoint must have no attributes"
        );

        // Required structure present.
        for tag in [
            "<Courses>",
            "<Course>",
            "<Track>",
            "<Lap>",
            "<Intensity>Active</Intensity>",
            "<CoursePoint><Name>Start</Name>",
            "<PointType>Generic</PointType>",
            "<AltitudeMeters>10.00</AltitudeMeters>",
        ] {
            assert!(tcx.xml.contains(tag), "missing: {tag}");
        }

        // Name comes from the GPX metadata name.
        assert!(tcx.xml.contains("<Name>Test Route</Name>"), "course name");
    }

    #[test]
    fn cumulative_distance_is_monotonic_and_correct() {
        let tcx = gpx_to_tcx(SYNTHETIC_GPX, "route.gpx").unwrap();

        // Take only the Track section so we read Trackpoint DistanceMeters (excluding the Lap total).
        let track = tcx
            .xml
            .split("<Track>")
            .nth(1)
            .and_then(|s| s.split("</Track>").next())
            .expect("Track section");

        // Extract DistanceMeters values in order.
        let mut dists = Vec::new();
        for chunk in track.split("<DistanceMeters>").skip(1) {
            let v: f64 = chunk
                .split("</DistanceMeters>")
                .next()
                .unwrap()
                .parse()
                .unwrap();
            dists.push(v);
        }
        assert_eq!(dists.len(), 5, "trackpoint distance count");
        assert_eq!(dists[0], 0.0, "first point distance is 0");
        for w in dists.windows(2) {
            assert!(w[1] >= w[0], "distance not monotonic: {} -> {}", w[0], w[1]);
            assert!(
                (w[1] - w[0] - SEGMENT_M).abs() < 1.0,
                "segment distance {} differs from expected",
                w[1] - w[0]
            );
        }
        // Total = 4 * SEGMENT_M ≈ 4447.80 m.
        let total = *dists.last().unwrap();
        assert!(
            (total - 4.0 * SEGMENT_M).abs() < 1.0,
            "total distance {total} mismatch"
        );
    }

    #[test]
    fn name_falls_back_to_filename_when_no_metadata() {
        let gpx = b"<?xml version=\"1.0\"?>\
<gpx version=\"1.1\" creator=\"t\" xmlns=\"http://www.topografix.com/GPX/1/1\">\
<trk><trkseg>\
<trkpt lat=\"35.0\" lon=\"129.0\"></trkpt>\
<trkpt lat=\"35.001\" lon=\"129.001\"></trkpt>\
</trkseg></trk></gpx>";
        let tcx = gpx_to_tcx(gpx, "my ride.gpx").unwrap();
        assert!(
            tcx.xml.contains("<Name>my ride</Name>"),
            "filename stem fallback"
        );
        // No-elevation point -> AltitudeMeters omitted.
        assert!(
            !tcx.xml.contains("<AltitudeMeters>"),
            "no ele -> no AltitudeMeters"
        );
        assert_eq!(tcx.filename, "my ride");
    }

    #[test]
    fn rejects_gpx_without_points() {
        let gpx = b"<?xml version=\"1.0\"?>\
<gpx version=\"1.1\" creator=\"t\" xmlns=\"http://www.topografix.com/GPX/1/1\"></gpx>";
        assert!(gpx_to_tcx(gpx, "empty.gpx").is_err());
    }

    // Collect the Trackpoint <Time> values from inside <Track>...</Track>.
    fn trackpoint_times(xml: &str) -> Vec<String> {
        let track = xml
            .split("<Track>")
            .nth(1)
            .and_then(|s| s.split("</Track>").next())
            .expect("Track section");
        track
            .split("<Time>")
            .skip(1)
            .map(|c| c.split("</Time>").next().unwrap().to_string())
            .collect()
    }

    #[test]
    fn timestamps_strictly_increase_for_dense_and_duplicate_points() {
        // Points ~3 m apart (< SPEED_MPS) and an exact duplicate (index 1 == 2): all
        // would floor to the same whole second without the strict-increase clamp.
        let gpx = br#"<?xml version="1.0"?>
<gpx version="1.1" creator="t" xmlns="http://www.topografix.com/GPX/1/1">
<trk><trkseg>
<trkpt lat="35.000000" lon="129.000000"/>
<trkpt lat="35.000027" lon="129.000000"/>
<trkpt lat="35.000027" lon="129.000000"/>
<trkpt lat="35.000054" lon="129.000000"/>
<trkpt lat="35.000081" lon="129.000000"/>
</trkseg></trk></gpx>"#;
        let tcx = gpx_to_tcx(gpx, "dense.gpx").unwrap();

        // RFC3339 fixed-width strings compare lexicographically == chronologically.
        let times = trackpoint_times(&tcx.xml);
        assert_eq!(times.len(), 5, "trackpoint count");
        for w in times.windows(2) {
            assert!(
                w[1] > w[0],
                "times must strictly increase: {} !> {}",
                w[1],
                w[0]
            );
        }

        // Lap total equals the last Trackpoint's elapsed seconds (single source).
        // 4 increments of +1 s from index-driven clamp -> 4 s.
        assert!(
            tcx.xml.contains("<TotalTimeSeconds>4</TotalTimeSeconds>"),
            "Lap total derives from the strictly-increasing series"
        );
    }

    #[test]
    fn tolerates_nonstandard_metadata_and_extensions() {
        // Mirrors a kakaomap export (synthetic coordinates): <description> in
        // <metadata> is not GPX-standard, and <wpt>/<trkpt> carry custom
        // <extensions><kakaomap-meta> children. The lenient parser must ignore all
        // of it and still extract the track. A schema-strict parser rejected this.
        let gpx = br#"<?xml version="1.0" encoding="UTF-8"?>
<gpx xmlns="http://www.topografix.com/GPX/1/1" version="1.1" creator="kakaomap-route">
  <metadata>
    <name>kakaomap</name>
    <description>iOS,26.5,iPhone</description>
    <time>2026-05-26T05:21:20Z</time>
  </metadata>
  <wpt lat="35.000000" lon="129.000000">
    <name>start poi</name>
    <extensions><kakaomap-meta><route_type>START</route_type></kakaomap-meta></extensions>
  </wpt>
  <trk><trkseg>
    <trkpt lat="35.000000" lon="129.000000"><ele>10.0</ele></trkpt>
    <trkpt lat="35.010000" lon="129.000000"><ele>12.0</ele>
      <extensions><kakaomap-meta><line_type>NONE</line_type></kakaomap-meta></extensions></trkpt>
  </trkseg></trk>
</gpx>"#;
        let tcx = gpx_to_tcx(gpx, "kakao.gpx").unwrap();

        // The track (2 trkpt) wins over the 2 wpt POIs.
        assert_eq!(
            count_occurrences(&tcx.xml, "<Trackpoint>"),
            2,
            "track points extracted despite non-standard sibling elements"
        );
        assert!(
            tcx.xml.contains("<AltitudeMeters>10.00</AltitudeMeters>"),
            "trkpt elevation read past the <extensions>"
        );
        // Course name from <metadata><name>, not the <wpt> POI <name>.
        assert!(
            tcx.xml.contains("<Name>kakaomap</Name>"),
            "metadata name used, not the waypoint POI name"
        );
        // No custom element leaks into the output.
        assert!(
            !tcx.xml.contains("kakaomap-meta") && !tcx.xml.contains("route_type"),
            "non-standard elements must not appear in the TCX"
        );
    }

    #[test]
    fn falls_back_to_route_points_when_no_track() {
        // No <trk>: points must come from <rte>/<rtept>, with elevation carried.
        let gpx = br#"<?xml version="1.0"?>
<gpx version="1.1" creator="t" xmlns="http://www.topografix.com/GPX/1/1">
<rte>
<rtept lat="35.000000" lon="129.000000"><ele>5.0</ele></rtept>
<rtept lat="35.010000" lon="129.000000"><ele>6.0</ele></rtept>
</rte></gpx>"#;
        let tcx = gpx_to_tcx(gpx, "route-only.gpx").unwrap();
        assert_eq!(
            count_occurrences(&tcx.xml, "<Trackpoint>"),
            2,
            "route points become Trackpoints"
        );
        assert!(
            tcx.xml.contains("<AltitudeMeters>5.00</AltitudeMeters>"),
            "rtept elevation carried through"
        );
    }

    #[test]
    fn falls_back_to_waypoints_when_no_track_or_route() {
        // No <trk> and no <rte>: points must come from top-level <wpt>.
        let gpx = br#"<?xml version="1.0"?>
<gpx version="1.1" creator="t" xmlns="http://www.topografix.com/GPX/1/1">
<wpt lat="35.000000" lon="129.000000"></wpt>
<wpt lat="35.010000" lon="129.000000"></wpt>
</gpx>"#;
        let tcx = gpx_to_tcx(gpx, "waypoints.gpx").unwrap();
        assert_eq!(
            count_occurrences(&tcx.xml, "<Trackpoint>"),
            2,
            "waypoints become Trackpoints"
        );
    }

    #[test]
    fn escapes_special_chars_in_course_name() {
        // The GPX entities decode to `A & B < C`; the converter must re-escape them
        // so the emitted TCX stays well-formed (no raw & or < in element text).
        let gpx = br#"<?xml version="1.0"?>
<gpx version="1.1" creator="t" xmlns="http://www.topografix.com/GPX/1/1">
<metadata><name>A &amp; B &lt; C</name></metadata>
<trk><trkseg>
<trkpt lat="35.0" lon="129.0"/>
<trkpt lat="35.001" lon="129.001"/>
</trkseg></trk></gpx>"#;
        let tcx = gpx_to_tcx(gpx, "x.gpx").unwrap();
        assert!(
            tcx.xml.contains("<Name>A &amp; B &lt; C</Name>"),
            "special chars in the course name must be XML-escaped"
        );
    }

    #[test]
    fn clamps_long_course_name() {
        // A 60-char name must be capped at MAX_NAME_CHARS in the emitted <Name>.
        let long = "a".repeat(60);
        let gpx = format!(
            r#"<?xml version="1.0"?>
<gpx version="1.1" creator="t" xmlns="http://www.topografix.com/GPX/1/1">
<metadata><name>{long}</name></metadata>
<trk><trkseg>
<trkpt lat="35.0" lon="129.0"/>
<trkpt lat="35.001" lon="129.001"/>
</trkseg></trk></gpx>"#
        );
        let tcx = gpx_to_tcx(gpx.as_bytes(), "x.gpx").unwrap();
        let name = tcx
            .xml
            .split("<Name>")
            .nth(1)
            .and_then(|s| s.split("</Name>").next())
            .expect("course Name");
        assert_eq!(
            name.chars().count(),
            MAX_NAME_CHARS,
            "course name must be clamped to MAX_NAME_CHARS"
        );
    }

    #[test]
    fn clamp_name_truncates_by_char_not_byte() {
        // Multibyte input must truncate on char boundaries (no panic, no split byte).
        let clamped = clamp_name(&"가".repeat(60));
        assert_eq!(
            clamped.chars().count(),
            MAX_NAME_CHARS,
            "char-count truncation"
        );
        assert_eq!(
            clamped,
            "가".repeat(MAX_NAME_CHARS),
            "exactly MAX_NAME_CHARS multibyte chars"
        );
    }
}
