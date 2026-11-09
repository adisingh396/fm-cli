// FM Observatory web configuration. This points the deployed frontend at the
// running Rust backend through the Cloudflare tunnel. To use a different host,
// override it from the dashboard's "API endpoint" field or with
// `?api=https://your-api.example`.
window.FM_API_BASE = 'https://plasma-checks-league-trinity.trycloudflare.com';
