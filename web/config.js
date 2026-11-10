// FM Observatory web configuration. When served by the Rust backend itself
// (cargo run --release -- serve) the API is same-origin, so no base is
// needed. For a deployed frontend (e.g. Vercel), point this at a public URL
// of the backend (a Cloudflare tunnel, a box, any host running
// `fm serve`), or set it per-browser with the dashboard's "API endpoint"
// field or `?api=https://your-api.example`. Leave empty for same-origin.
window.FM_API_BASE = '';
