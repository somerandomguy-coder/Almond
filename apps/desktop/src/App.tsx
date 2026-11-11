import type { ReactNode } from "react";
import { HashRouter, Route, Routes } from "react-router";
import { I18nextProvider } from "react-i18next";
import type { i18n as I18n } from "i18next";
import { ApiProvider } from "./api/context";
import type { AlmondApi } from "./api/types";
import { Shell } from "./components/Shell";
import { Cookbook } from "./screens/Cookbook";
import { Devices } from "./screens/Devices";
import { Home } from "./screens/Home";
import { MeetingScreen } from "./screens/Meeting";
import { People } from "./screens/People";
import { PersonScreen } from "./screens/Person";
import { Settings } from "./screens/Settings";
import { AppStateProvider } from "./state/AppState";

export function AppRoutes() {
  return (
    <Routes>
      <Route element={<Shell />}>
        <Route index element={<Home />} />
        <Route path="meeting/:id" element={<MeetingScreen />} />
        <Route path="people" element={<People />} />
        <Route path="people/:id" element={<PersonScreen />} />
        <Route path="devices" element={<Devices />} />
        <Route path="settings" element={<Settings />} />
        <Route path="cookbook" element={<Cookbook />} />
        <Route path="*" element={<Home />} />
      </Route>
    </Routes>
  );
}

/** Providers without a router (tests wrap this in a MemoryRouter). */
export function AppProviders({ api, i18n, children }: { api: AlmondApi; i18n: I18n; children: ReactNode }) {
  return (
    <I18nextProvider i18n={i18n}>
      <ApiProvider api={api}>
        <AppStateProvider>{children}</AppStateProvider>
      </ApiProvider>
    </I18nextProvider>
  );
}

export default function App({ api, i18n }: { api: AlmondApi; i18n: I18n }) {
  // Hash routing works for Tauri's custom protocol and Android asset loading alike.
  return (
    <AppProviders api={api} i18n={i18n}>
      <HashRouter>
        <AppRoutes />
      </HashRouter>
    </AppProviders>
  );
}
