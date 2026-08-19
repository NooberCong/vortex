import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

export default {
  preprocess: vitePreprocess(),
  compilerOptions: {
    // Runes everywhere. Mixing the two reactivity models in one app is how a Svelte 5
    // codebase ends up with two mental models and a class of bug in the seam.
    runes: true,
  },
};
