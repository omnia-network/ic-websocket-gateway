import IcWebSocket from "./icWebsocket";

const backendCanisterId = process.env.IC_WEBSOCKET_BACKEND_CANISTER_ID || "";
const gatewayAddress = "ws://127.0.0.1:8080";
const url = "http://127.0.0.1:4943";
const localTest = true;
const persistKey = false;
// const url = "https://ic0.app";
// const localTest = false;

document.querySelector<HTMLDivElement>('#app')!.innerHTML = `
  <div>
    <h1>IC WebSocket</h1>
    <div class="notifications-outer">
      <div id="notifications" class="notifications"></div>
    </div>
  </div>
`

const ws = new IcWebSocket(backendCanisterId, gatewayAddress, url, localTest, persistKey);

console.log(ws);
