import type { ActorMethod } from '@dfinity/agent';

export type ClientPublicKey = Uint8Array | number[];

export interface RelayedClientMessage {
  'sig': Uint8Array | number[],
  'content': Uint8Array | number[],
}

export interface DirectClientMessage {
  'client_key': ClientPublicKey,
  'message': Uint8Array | number[],
}

export type CanisterIncomingMessage = {
  'IcWebSocketEstablished': ClientPublicKey
} |
{ 'DirectlyFromClient': DirectClientMessage } |
{ 'RelayedByGateway': RelayedClientMessage };

export interface CanisterWsMessageArguments { 'msg': CanisterIncomingMessage }
export type CanisterWsMessageResult = { 'Ok': null } |
{ 'Err': string };

export interface CanisterWsRegisterArguments { 'client_key': ClientPublicKey }
export type CanisterWsRegisterResult = { 'Ok': null } |
{ 'Err': string };

export type ActorService = {
  'ws_message': ActorMethod<
    [CanisterWsMessageArguments],
    CanisterWsMessageResult
  >,
  'ws_register': ActorMethod<
    [CanisterWsRegisterArguments],
    CanisterWsRegisterResult
  >,
};