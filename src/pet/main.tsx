import React from "react";
import ReactDOM from "react-dom/client";
import Pet from "./Pet";
import "./pet.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <Pet />
  </React.StrictMode>,
);
