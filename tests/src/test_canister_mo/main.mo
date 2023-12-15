import TrieSet "mo:base/TrieSet";
import Principal "mo:base/Principal";
import Time "mo:base/Time";
import Debug "mo:base/Debug";
import Nat64 "mo:base/Nat64";

import IcWebSocketCdk "mo:ic-websocket-cdk";
import IcWebSocketCdkState "mo:ic-websocket-cdk/State";
import IcWebSocketCdkTypes "mo:ic-websocket-cdk/Types";

actor class TestCanister() {

  Debug.print("TestCanister actor started");

  let params = IcWebSocketCdkTypes.WsInitParams(null, null);

  let ws_state = IcWebSocketCdkState.IcWebSocketState(params);

  type ClientPrincipal = IcWebSocketCdk.ClientPrincipal;

  type AppMessage = {
    text : Text;
    timestamp : Nat64;
  };

  var clients_connected : TrieSet.Set<ClientPrincipal> = TrieSet.empty();

  func on_open(args : IcWebSocketCdk.OnOpenCallbackArgs) : async () {
    clients_connected := TrieSet.put(clients_connected, args.client_principal, Principal.hash(args.client_principal), Principal.equal);
    Debug.print("[on_open] # clients connected: " # debug_show (TrieSet.size(clients_connected)));
    let msg : AppMessage = {
      text = "ping";
      timestamp = Nat64.fromIntWrap(Time.now());
    };
    await send_app_message(args.client_principal, msg);
  };

  func on_message(args : IcWebSocketCdk.OnMessageCallbackArgs) : async () {
    let app_msg : ?AppMessage = from_candid (args.message);
    switch (app_msg) {
      case (null) {
        Debug.print("Error decoding message");
      };
      case (?msg) {
        let new_msg : AppMessage = {
          text = msg.text # " ping";
          timestamp = Nat64.fromIntWrap(Time.now());
        };
        Debug.print("[on_message] Received message");
        await send_app_message(args.client_principal, new_msg);
      };
    };
  };

  func on_close(args : IcWebSocketCdk.OnCloseCallbackArgs) : async () {
    clients_connected := TrieSet.delete(clients_connected, args.client_principal, Principal.hash(args.client_principal), Principal.equal);
    Debug.print("[on_close] # clients connected: " # debug_show (TrieSet.size(clients_connected)));
  };

  func send_app_message(to_principal : Principal, msg : AppMessage) : async () {
    let msg_bytes = to_candid (msg);
    switch (await IcWebSocketCdk.send(ws_state, to_principal, msg_bytes)) {
      case (#Err(err)) {
        Debug.print("ECould not send message: " # debug_show (err));
      };
      case (#Ok(_)) {
        Debug.print("Message sent");
      };
    };
  };

  let handlers = IcWebSocketCdkTypes.WsHandlers(
    ?on_open,
    ?on_message,
    ?on_close,
  );

  let ws = IcWebSocketCdk.IcWebSocket(ws_state, params, handlers);

  // method called by the WS Gateway after receiving FirstMessage from the client
  public shared ({ caller }) func ws_open(args : IcWebSocketCdk.CanisterWsOpenArguments) : async IcWebSocketCdk.CanisterWsOpenResult {
    await ws.ws_open(caller, args);
  };

  // method called by the Ws Gateway when closing the IcWebSocket connection
  public shared ({ caller }) func ws_close(args : IcWebSocketCdk.CanisterWsCloseArguments) : async IcWebSocketCdk.CanisterWsCloseResult {
    await ws.ws_close(caller, args);
  };

  // method called by the WS Gateway to send a message of type GatewayMessage to the canister
  public shared ({ caller }) func ws_message(args : IcWebSocketCdk.CanisterWsMessageArguments, msg_type : ?AppMessage) : async IcWebSocketCdk.CanisterWsMessageResult {
    await ws.ws_message(caller, args, msg_type);
  };

  // method called by the WS Gateway to get messages for all the clients it serves
  public shared query ({ caller }) func ws_get_messages(args : IcWebSocketCdk.CanisterWsGetMessagesArguments) : async IcWebSocketCdk.CanisterWsGetMessagesResult {
    ws.ws_get_messages(caller, args);
  };

  //// Debug/tests methods
  // send a message to the client, usually called by the canister itself
  public shared func send(client_principal : IcWebSocketCdk.ClientPrincipal, msg_bytes : Blob) : async IcWebSocketCdk.CanisterSendResult {
    await IcWebSocketCdk.send(ws_state, client_principal, msg_bytes);
  };
};
