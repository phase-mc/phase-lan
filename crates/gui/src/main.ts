import { mount } from "svelte";
// Fonts are bundled inside the program: the window works offline.
// Inter — the whole interface (Latin and Cyrillic), JetBrains Mono — technical values only.
import "./fonts.css";
import "@fontsource/jetbrains-mono/latin-400.css";
import "@fontsource/jetbrains-mono/latin-500.css";
import "@fontsource/jetbrains-mono/cyrillic-400.css";
import "./app.css";
// The theme is set on <html> when settings are created — before the first frame.
import "./lib/settings.svelte";
import App from "./App.svelte";
import { installGuard } from "./lib/guard";

// Before the first frame: no browser menu, shortcuts or dragging.
installGuard();

const target = document.getElementById("app");
if (!target) throw new Error("missing #app element");

export default mount(App, { target });
