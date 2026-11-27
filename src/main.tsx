import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { MeetingPopup } from "./components/MeetingPopup";
import { BackendContext, createDefaultBackend } from "./lib/backend";
import { installDebugLogging } from "./lib/debugLog";
import "./styles.css";

const backend = createDefaultBackend();
// 会議検出のポップアップは同じ画面のコードを「#/popup」で開く
const isPopup = window.location.hash.startsWith("#/popup");
if (isPopup) document.documentElement.classList.add("is-popup");
installDebugLogging(backend, window, isPopup ? "popup" : "main");

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <BackendContext.Provider value={backend}>{isPopup ? <MeetingPopup /> : <App />}</BackendContext.Provider>
  </StrictMode>,
);
