const { contextBridge } = require("electron");

contextBridge.exposeInMainWorld("vfDesktop", { isDesktop: true, platform: process.platform });
