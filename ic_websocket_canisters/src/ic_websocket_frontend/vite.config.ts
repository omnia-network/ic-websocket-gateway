import { defineConfig } from 'vite';
import EnvironmentPlugin from 'vite-plugin-environment';
import { config } from 'dotenv';

config({
  path: "../../.env",
});

const isDevelopment = process.env.NODE_ENV !== "production";

// https://vitejs.dev/config/
export default defineConfig({
  mode: isDevelopment ? "development" : "production",
  plugins: [
    EnvironmentPlugin('all', {
      prefix: 'IC_WEBSOCKET',
    }),
    EnvironmentPlugin('all', {
      prefix: 'DFX',
    }),
    EnvironmentPlugin('all', {
      prefix: 'INTERNET_IDENTITY',
    }),
    EnvironmentPlugin({
      NODE_ENV: process.env.NODE_ENV,
    }),
  ],
  server: {
    proxy: {
      "/api": "http://127.0.0.1:4943",
    },
  },
  optimizeDeps: {
    esbuildOptions: {
      // Node.js global to browser globalThis
      define: {
        global: "globalThis",
      },
    },
  },
});