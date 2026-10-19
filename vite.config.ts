import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { transform } from "oxc-transform-react";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// Runs the Oxc React Compiler before @vitejs/plugin-react. JSX is preserved so
// the regular pipeline still handles the JSX transform and Fast Refresh.
// https://oxc.rs/docs/guide/usage/transformer/react-compiler.html
function reactCompiler(): Plugin {
  return {
    name: "oxc-react-compiler",
    enforce: "pre",
    async transform(code, id) {
      const [filename] = id.split("?");
      if (!/\.[jt]sx$/.test(filename) || filename.includes("/node_modules/")) return;

      const result = await transform(filename, code, {
        sourcemap: true,
        jsx: "preserve",
        reactCompiler: { target: "19" },
      });
      if (result.fatal) {
        this.error(result.errors.map((e) => e.codeframe ?? e.message).join("\n"));
      }
      for (const e of result.errors) this.warn(e.message);

      return { code: result.code, map: result.map };
    },
  };
}

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [reactCompiler(), react(), tailwindcss()],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
