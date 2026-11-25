import { createRoot } from "react-dom/client";
import "./index.css";
import "./lib/theme";
import App from "./App";

declare global {
  interface Window { vfDesktop?: { isDesktop: boolean; platform: string } }
}
if (window.vfDesktop?.isDesktop) document.documentElement.classList.add("desktop");

createRoot(document.getElementById("root")!).render(<App />);
