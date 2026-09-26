import React from "react";
import ReactDOM from "react-dom/client";

document.addEventListener("contextmenu", (e) => e.preventDefault());

import App from "./App";
import { applyStoredThemeClass } from "./theme";
import { ThemeProvider } from "./ThemeContext";
import "./i18n";
import "./index.css";

applyStoredThemeClass();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ThemeProvider>
      <App />
    </ThemeProvider>
  </React.StrictMode>,
);
