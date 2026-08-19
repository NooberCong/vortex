import "@fontsource-variable/instrument-sans/wght.css";
import "@fontsource/commit-mono/latin-400.css";
import "@fontsource/commit-mono/latin-500.css";
import "./app.css";

import { mount } from "svelte";

import App from "./App.svelte";

/**
 * The entry point.
 *
 * Both typefaces are bundled rather than fetched. *Instrument Sans* is variable, so one
 * file covers every weight the interface uses; *Commit Mono* ships as two static cuts
 * because every number in the app is 400 or 500 and nothing else. Neither is loaded from a
 * network: a download manager that needs the internet to render its own list would be an
 * embarrassing kind of wrong, and the strict CSP in `tauri.conf.json` would refuse it
 * anyway.
 */
const app = mount(App, { target: document.getElementById("app")! });

export default app;
