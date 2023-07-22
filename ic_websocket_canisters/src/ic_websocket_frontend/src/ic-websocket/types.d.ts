export interface CanisterMessage {
  'client_key': Uint8Array | number[],
  'message': Uint8Array | number[],
}

export interface ClientMessage {
  'sig': Uint8Array | number[],
  'content': Uint8Array | number[],
}

export type GatewayMessage = { 'RelayedFromClient': ClientMessage } |
{ 'IcWebSocketEstablished': Uint8Array | number[] } |
{ 'DirectlyFromClient': CanisterMessage };

export type ActorService = {
  'ws_message': ActorMethod<[GatewayMessage], WsGenericResult>,
  'ws_register': ActorMethod<[Uint8Array | number[]], undefined>,
};