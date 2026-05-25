# gpx-converter

A self-hosted web page that **converts GPX route files to TCX course files**. It
solves the problem where an older cycling computer (e.g. Trimm Two) cannot load
GPX routes but reads TCX/FIT fine. Meant for personal use behind a VPN.

- Open the page on your phone, upload a GPX, and the **converted TCX downloads
  immediately**.
- Conversion runs on the server (Rust); files are never sent to any external
  service.
- Single binary — no runtime dependencies.

## Build

```sh
cargo build --release
# binary: target/release/gpx-converter
```

## Run

```sh
# default port 8080, binds to all interfaces (0.0.0.0)
./target/release/gpx-converter

# custom port
PORT=9000 ./target/release/gpx-converter
```

On start it prints `gpx-converter listening on http://0.0.0.0:8080`.

## Access from your phone

1. Run the binary on a machine reachable inside your VPN (home server / PC /
   Raspberry Pi, etc.).
2. Connect your phone to the same VPN.
3. Open `http://<that machine's VPN IP>:8080` in the browser.
4. Pick a GPX file → **Convert & Download** → the TCX is downloaded.
5. Copy the TCX onto the cycling computer and load the route.

## Conversion rules

The output mirrors a Course file produced by a map app, reverse-engineered to
match its format.

- GPX track points (`<trk>/<trkseg>/<trkpt>`) become course Trackpoints in order.
  If there is no track, it falls back to `<rte>/<rtept>`, then `<wpt>`.
- Distance between points is accumulated with the **Haversine** formula
  (`DistanceMeters`).
- Timestamps (`Time`) are synthesized assuming a constant **20 km/h** (the
  source GPX has no time). Absolute time is irrelevant for course following, so
  the start is the current UTC at conversion time.
- The `<Lap>` carries total time/distance and begin/end positions; a single
  `CoursePoint(Start)` is emitted at the first point.
- Course name: GPX `<metadata><name>` → first track name → uploaded filename.
- Output is a **standard TCX subset** with the map-app-specific non-standard
  attributes (`sectionIndex`, etc.) stripped.

## Run as a service (optional)

Example `systemd` unit (`/etc/systemd/system/gpx-converter.service`):

```ini
[Unit]
Description=GPX to TCX converter
After=network.target

[Service]
ExecStart=/path/to/gpx-converter
Environment=PORT=8080
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl enable --now gpx-converter
```

## Development

```sh
cargo fmt --all
cargo clippy -p gpx-converter --all-targets -- -D warnings
cargo nextest run -p gpx-converter

# local manual check
cargo run &
curl -F "file=@your-route.gpx" http://127.0.0.1:8080/convert -o out.tcx
```

## Out of scope

- FIT output (TCX only for now). The device reads TCX routes, which is enough.
- Authentication / login (access control is handled by the VPN).
