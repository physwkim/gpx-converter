//! GPX -> TCX Course 변환.
//!
//! 출력 형식은 맵앱이 만든 TCX Course 파일을 역공학해 맞췄다.
//! 핵심 규칙은 거리 누적(Haversine) + 20 km/h 등속으로 시각을 합성하는 것.
//! 맵앱 전용 비표준 속성(sectionIndex 등)은 생성하지 않는다 — 표준 TCX 부분집합.

use chrono::{SecondsFormat, TimeDelta, Utc};
use std::io::Cursor;

/// 예제가 사용한 속도. 거리/시간 합성에 쓰인다 (= 20 km/h).
/// 검증: point5 거리 382.62 m / 5.5556 = 68.9 s, 시작 22:04:07 + 68 s = 22:05:15 (예제 일치).
const SPEED_MPS: f64 = 5.5556;

/// 코스 이름 최대 길이(문자 수). 일부 기기가 긴 이름을 자르므로 미리 제한한다.
const MAX_NAME_CHARS: usize = 50;

struct Pt {
    lat: f64,
    lon: f64,
    ele: Option<f64>,
}

/// 변환 결과: 다운로드 파일명 stem(확장자 제외)과 TCX XML 본문.
pub struct Tcx {
    pub filename: String,
    pub xml: String,
}

/// GPX 바이트를 TCX Course XML로 변환한다.
///
/// `upload_name`은 업로드된 원본 파일명(이름 폴백 및 다운로드 파일명 stem에 사용).
pub fn gpx_to_tcx(bytes: &[u8], upload_name: &str) -> Result<Tcx, String> {
    let gpx = gpx::read(Cursor::new(bytes)).map_err(|e| format!("GPX 파싱 실패: {e}"))?;

    // 점 추출: 트랙 -> 라우트 -> 웨이포인트 순으로 폴백.
    let mut pts: Vec<Pt> = Vec::new();
    for trk in &gpx.tracks {
        for seg in &trk.segments {
            for wp in &seg.points {
                let p = wp.point();
                pts.push(Pt {
                    lat: p.y(),
                    lon: p.x(),
                    ele: wp.elevation,
                });
            }
        }
    }
    if pts.is_empty() {
        for rte in &gpx.routes {
            for wp in &rte.points {
                let p = wp.point();
                pts.push(Pt {
                    lat: p.y(),
                    lon: p.x(),
                    ele: wp.elevation,
                });
            }
        }
    }
    if pts.is_empty() {
        for wp in &gpx.waypoints {
            let p = wp.point();
            pts.push(Pt {
                lat: p.y(),
                lon: p.x(),
                ele: wp.elevation,
            });
        }
    }
    if pts.is_empty() {
        return Err("GPX에 좌표 점이 없습니다.".to_string());
    }

    // 이름: metadata.name -> 첫 트랙 name -> 파일명 stem -> "Course".
    let stem = file_stem(upload_name);
    let name = gpx
        .metadata
        .as_ref()
        .and_then(|m| m.name.clone())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            gpx.tracks
                .first()
                .and_then(|t| t.name.clone())
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| (!stem.is_empty()).then(|| stem.clone()))
        .unwrap_or_else(|| "Course".to_string());

    // 누적 거리(Haversine). 첫 점은 0.
    let mut cum = vec![0.0_f64; pts.len()];
    for i in 1..pts.len() {
        let d = haversine(pts[i - 1].lat, pts[i - 1].lon, pts[i].lat, pts[i].lon);
        cum[i] = cum[i - 1] + d;
    }
    let total = *cum.last().unwrap();

    // 시작 시각은 변환 시점의 현재 UTC. 코스 추종에는 절대 시각이 무의미하므로 무방.
    let start = Utc::now();
    let at = |secs: f64| -> String {
        let ms = (secs * 1000.0) as i64;
        (start + TimeDelta::milliseconds(ms)).to_rfc3339_opts(SecondsFormat::Secs, true)
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

    // Lap: 총시간/총거리/시작·끝 좌표.
    s.push_str("<Lap>\n");
    s.push_str(&format!(
        "<TotalTimeSeconds>{:.0}</TotalTimeSeconds>\n",
        total / SPEED_MPS
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

    // Track: 점마다 Trackpoint 하나.
    s.push_str("<Track>\n");
    for (i, p) in pts.iter().enumerate() {
        s.push_str("<Trackpoint>");
        s.push_str(&format!("<Time>{}</Time>", at(cum[i] / SPEED_MPS)));
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

    // CoursePoint: 시작점 하나(Generic). 예제와 동일한 최소 구성.
    s.push_str("<CoursePoint><Name>Start</Name>");
    s.push_str(&format!("<Time>{}</Time>", at(0.0)));
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

/// 두 위경도 좌표 사이의 대원 거리(m). 외부 크레이트 없이 직접 구현.
fn haversine(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R: f64 = 6_371_000.0; // 지구 평균 반지름(m)
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    R * c
}

/// XML 텍스트 이스케이프(요소 내용·이름용).
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

/// 경로 구분자를 제거하고 마지막 확장자를 떼어낸 파일명 stem.
fn file_stem(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match base.rsplit_once('.') {
        Some((stem, _ext)) if !stem.is_empty() => stem.to_string(),
        _ => base.to_string(),
    }
}

/// 앞뒤 공백 제거 후 문자 수 기준으로 이름을 자른다(UTF-8 경계 안전).
fn clamp_name(s: &str) -> String {
    s.trim().chars().take(MAX_NAME_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // 자오선(같은 경도)을 따라 위도만 0.01°씩 증가하는 합성 경로. 개인 데이터 없음.
    // 자오선에서 Haversine 거리는 R*Δlat 와 정확히 같으므로 거리가 결정적이다:
    // 0.01° = 6_371_000 * 0.01 * π/180 ≈ 1111.95 m, 4구간 → ≈ 4447.80 m.
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
        let tcx = gpx_to_tcx(SYNTHETIC_GPX, "route.gpx").expect("변환 성공");

        // trkpt 5개 -> Trackpoint 5개. 비표준 속성 없는 깨끗한 형태.
        assert_eq!(
            count_occurrences(&tcx.xml, "<Trackpoint>"),
            5,
            "Trackpoint 개수"
        );
        assert!(
            tcx.xml.contains("<Trackpoint><Time>"),
            "Trackpoint에 속성이 붙음"
        );

        // 필수 구조 존재.
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
            assert!(tcx.xml.contains(tag), "누락: {tag}");
        }

        // 이름은 GPX metadata의 name.
        assert!(tcx.xml.contains("<Name>Test Route</Name>"), "코스 이름");
    }

    #[test]
    fn cumulative_distance_is_monotonic_and_correct() {
        let tcx = gpx_to_tcx(SYNTHETIC_GPX, "route.gpx").unwrap();

        // Track 구간만 떼어내 Trackpoint의 DistanceMeters만 본다(Lap 총거리 제외).
        let track = tcx
            .xml
            .split("<Track>")
            .nth(1)
            .and_then(|s| s.split("</Track>").next())
            .expect("Track 구간");

        // DistanceMeters 값을 순서대로 추출.
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
        assert_eq!(dists.len(), 5, "Trackpoint 거리 개수");
        assert_eq!(dists[0], 0.0, "첫 점 거리 0");
        for w in dists.windows(2) {
            assert!(w[1] >= w[0], "거리 단조 증가 위반: {} -> {}", w[0], w[1]);
            assert!(
                (w[1] - w[0] - SEGMENT_M).abs() < 1.0,
                "구간 거리 {} 가 예상과 다름",
                w[1] - w[0]
            );
        }
        // 총거리 = 4 * SEGMENT_M ≈ 4447.80 m.
        let total = *dists.last().unwrap();
        assert!(
            (total - 4.0 * SEGMENT_M).abs() < 1.0,
            "총거리 {total} 불일치"
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
        assert!(tcx.xml.contains("<Name>my ride</Name>"), "파일명 stem 폴백");
        // 고도 없는 점 -> AltitudeMeters 미출력.
        assert!(
            !tcx.xml.contains("<AltitudeMeters>"),
            "ele 없으면 AltitudeMeters 없음"
        );
        assert_eq!(tcx.filename, "my ride");
    }

    #[test]
    fn rejects_gpx_without_points() {
        let gpx = b"<?xml version=\"1.0\"?>\
<gpx version=\"1.1\" creator=\"t\" xmlns=\"http://www.topografix.com/GPX/1/1\"></gpx>";
        assert!(gpx_to_tcx(gpx, "empty.gpx").is_err());
    }
}
