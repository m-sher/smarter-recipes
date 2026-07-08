import "./styles.css";
import { createApi } from "./bridge";
import { initialState, loadPageData, render, type AppState, type Page } from "./app";

const root = document.querySelector<HTMLElement>("#app");
if (!root) {
  throw new Error("#app missing");
}

const api = createApi();
window.__SR_API__ = api;

let state: AppState = initialState();

function paint(): void {
  render(root!, state, (page) => {
    void navigate(page);
  });
}

async function navigate(page: Page): Promise<void> {
  state = { ...state, page, loading: true, error: null };
  paint();
  const patch = await loadPageData(api, page);
  state = { ...state, ...patch, page };
  paint();
}

void navigate("home");
