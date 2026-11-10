import "@fontsource-variable/inter";
import "./styles.css";
import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { createApi } from "./api/context";
import { initI18n } from "./i18n";

const i18n = initI18n();
const api = createApi();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App api={api} i18n={i18n} />
  </React.StrictMode>,
);
