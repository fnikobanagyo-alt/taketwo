// Records where the TakeTwo project lives, so the app can start its engine.
const fs = require("fs");
const path = require("path");
const root = path.resolve(__dirname, "..");
fs.writeFileSync(path.join(__dirname, "config.json"), JSON.stringify({ root }, null, 2));
console.log("TakeTwo root:", root);
