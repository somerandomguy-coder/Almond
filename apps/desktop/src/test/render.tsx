import { render } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { AppProviders, AppRoutes } from "../App";
import { MockApi, type MockOptions } from "../api/mock/MockApi";
import { initI18n, type UiLang } from "../i18n";

/** Render the whole app at `path` against a fast MockApi. */
export function renderApp(path = "/", opts: MockOptions & { lang?: UiLang } = {}) {
  const api = new MockApi({ speed: 25, reminderAfterMs: null, ...opts });
  const i18n = initI18n(opts.lang ?? "en");
  const utils = render(
    <AppProviders api={api} i18n={i18n}>
      <MemoryRouter initialEntries={[path]}>
        <AppRoutes />
      </MemoryRouter>
    </AppProviders>,
  );
  return { api, i18n, ...utils };
}
