import {
  ActorSubclass,
  Cbor,
  HttpAgent,
} from "@dfinity/agent";
import { Principal } from "@dfinity/principal";
import * as ed from '@noble/ed25519';
import { isMessageBodyValid } from "./utils";
import type { ActorService, CanisterWsMessageArguments, ClientPublicKey } from "./actor";

const CLIENT_SECRET_KEY_STORAGE_KEY = "ic_websocket_client_secret_key";

type ClientIncomingMessage = {
  key: string;
  cert: ArrayBuffer;
  tree: ArrayBuffer;
  val: ArrayBuffer;
}

type ClientIncomingMessageContent = {
  client_key: ClientPublicKey;
  sequence_num: number;
  timestamp: number;
  message: ArrayBuffer;
};

export type IcWebSocketConfig<T extends ActorService> = {
  /**
   * The canister id of the canister to open the websocket to.
   */
  canisterId: string;
  /**
   * The canister actor class.
   * 
   * It must implement the methods:
   * - `ws_register`
   * - `ws_message`
   */
  canisterActor: ActorSubclass<T>
  /**
   * The IC network url to use for the HttpAgent. It can be a local replica (e.g. http://localhost:4943) or the IC mainnet (https://ic0.io).
   */
  networkUrl: string;
  /**
   * If `true`, it means that the network is a local replica and the HttpAgent will fetch the root key.
   */
  localTest: boolean;
  /**
   * If `true`, the secret key will be stored in local storage and reused on subsequent page loads.
   */
  persistKey?: boolean;
};

type WsParameters = ConstructorParameters<typeof WebSocket>;

export default class IcWebSocket<T extends ActorService> {
  readonly canisterId: Principal;
  readonly agent: HttpAgent;
  readonly canisterActor: ActorSubclass<T>;
  private wsInstance: WebSocket;
  private secretKey: Uint8Array | string;
  private nextReceivedNum: number;
  private isConnectionOpen = false;

  onclose: ((this: IcWebSocket<T>, ev: CloseEvent) => any) | null = null;
  onerror: ((this: IcWebSocket<T>, ev: ErrorEvent) => any) | null = null;
  onmessage: ((this: IcWebSocket<T>, ev: MessageEvent<any>) => any) | null = null;
  onopen: ((this: IcWebSocket<T>, ev: Event) => any) | null = null;

  /**
   * Creates a new IcWebSocket instance.
   * @param url The gateway address.
   * @param protocols The protocols to use in the WebSocket.
   * @param config The IcWebSocket configuration.
   */
  constructor(url: WsParameters[0], protocols: WsParameters[1], config: IcWebSocketConfig<T>) {
    this.canisterId = Principal.fromText(config.canisterId);

    if (!config.canisterActor.ws_register) {
      throw new Error("Canister actor does not implement the ws_register method");
    }

    if (!config.canisterActor.ws_message) {
      throw new Error("Canister actor does not implement the ws_message method");
    }

    if (config.persistKey) {
      // attempt to load the secret key from local storage (stored in hex format)
      const storedKey = localStorage.getItem(CLIENT_SECRET_KEY_STORAGE_KEY);

      if (storedKey) {
        console.log("Using stored key");
        this.secretKey = storedKey;
      } else {
        console.log("Generating and storing new key");
        this.secretKey = ed.utils.randomPrivateKey(); // Generate new key for this websocket connection.
        localStorage.setItem(CLIENT_SECRET_KEY_STORAGE_KEY, ed.etc.bytesToHex(this.secretKey));
      }
    } else {
      console.log("Generating new key");
      this.secretKey = ed.utils.randomPrivateKey(); // Generate new key for this websocket connection.
    }

    this.canisterActor = config.canisterActor;

    this.nextReceivedNum = 0; // Received signed messages need to come in the correct order, with sequence numbers 0, 1, 2...
    this.wsInstance = new WebSocket(url, protocols); // Gateway address. Here localhost to reproduce the demo.
    this.wsInstance.binaryType = "arraybuffer";
    this._bindWsEvents();

    this.agent = new HttpAgent({ host: config.networkUrl });
    if (config.localTest) {
      this.agent.fetchRootKey();
    }
  }

  async send(data: any) {
    if (!this.isConnectionOpen) {
      throw new Error("Connection is not open");
    }

    try {
      // We send the message directly to the canister, not to the gateway
      const message = await this._makeApplicationMessage(data);

      const sendResult = await this.canisterActor.ws_message(message);

      if ("Err" in sendResult) {
        throw new Error(sendResult.Err);
      }
    } catch (error) {
      console.error("[send] Error:", error);
      if (this.onerror) {
        this.onerror.call(this, new ErrorEvent("error", { error }));
      }
    }
  }

  close() {
    this.wsInstance.close();
  }

  private _bindWsEvents() {
    this.wsInstance.onopen = this._onWsOpen.bind(this);
    this.wsInstance.onmessage = this._onWsMessage.bind(this);
    this.wsInstance.onclose = this._onWsClose.bind(this);
    this.wsInstance.onerror = this._onWsError.bind(this);
  }

  private async _onWsOpen() {
    console.log("[open] WS opened");

    try {
      // Send the first message
      const wsMessage = await this._makeFirstMessage();
      this.wsInstance.send(wsMessage);

      console.log("[open] First message sent");
    } catch (error) {
      console.error("[open] Error:", error);
      // if the first message fails, we can't continue
      this.wsInstance.close(3000, "First message failed");
    }

    // the onopen callback for the user is called when the first confirmation message is received from the canister
    // which happens in the _onWsMessage function
  }

  private async _onWsMessage(event: MessageEvent<ArrayBuffer>) {
    if (this.nextReceivedNum === 0) {
      // first received message
      console.log('[message]: first received message content:', event.data);
      this.nextReceivedNum += 1;

      console.log("[open] Connection opened");

      // We are ready to send messages 
      this.isConnectionOpen = true;

      if (this.onopen) {
        this.onopen.call(this, new Event("open"));
      }
    } else {
      const incomingMessage = this._decodeIncomingMessage(event.data);
      const incomingContent = this._getContentFromIncomingMessage(incomingMessage);

      const isSequenceNumValid = this._isIncomingMessageSequenceNumberValid(incomingContent);
      if (!isSequenceNumValid) {
        // TODO: handle out of order messages
        if (this.onerror) {
          this.onerror.call(this, new ErrorEvent("error", {
            error: `Received message sequence number does not match next expected value (${this.nextReceivedNum}). Message ignored.`,
          }));
        }
        return;
      }
      // Increment the next expected sequence number
      this.nextReceivedNum += 1;

      const isValidMessage = await this._isIncomingMessageValid(incomingMessage);
      if (!isValidMessage) {
        if (this.onerror) {
          this.onerror.call(this, new ErrorEvent("error", { error: "Certificate validation failed" }));
        }
        return;
      }

      this._inspectIncomingMessageTimestamp(incomingContent);

      // Message has been verified
      const appMsg = this._getApplicationMessageFromIncomingContent(incomingContent);
      console.log("[message] Message from canister");

      if (this.onmessage) {
        this.onmessage.call(this, new MessageEvent("message", {
          data: appMsg,
        }));
      }
    }
  }

  private _onWsClose(event: CloseEvent) {
    if (event.wasClean) {
      console.log(
        `[close] Connection closed, code=${event.code} reason=${event.reason}`
      );
    } else {
      console.log("[close] Connection died");
    }

    if (this.onclose) {
      this.onclose.call(this, event);
    }
  }

  private _onWsError(error: Event) {
    console.log(`[error]`, error);

    if (this.onerror) {
      this.onerror.call(this, new ErrorEvent("error", { error }));
    }
  }

  private async _getPublicKey(): Promise<Uint8Array> {
    return ed.getPublicKeyAsync(this.secretKey);
  }

  private async _registerPublicKeyOnCanister(): Promise<Uint8Array | undefined> {
    const publicKey = await this._getPublicKey();
    // Put the public key in the canister
    await this.canisterActor.ws_register({
      client_key: publicKey,
    });

    return publicKey;
  }

  private async _getSignedMessage(buf: ArrayBuffer | Uint8Array) {
    // Sign the message so that the gateway can verify canister and client ids match
    const toSign = new Uint8Array(buf);
    const sig = await ed.signAsync(toSign, this.secretKey);

    // Final signed websocket message
    const message = {
      content: toSign,
      sig: sig,
    };

    return message;
  }

  private async _makeFirstMessage() {
    const publicKey = await this._registerPublicKeyOnCanister();

    // Send the first message with client and canister id
    const cborContent = Cbor.encode({
      client_key: publicKey,
      canister_id: this.canisterId,
    });

    const signedMessage = await this._getSignedMessage(cborContent);

    // Send the first message
    const wsMessage = Cbor.encode(signedMessage);

    return wsMessage;
  }

  private _decodeIncomingMessage(buf: ArrayBuffer): ClientIncomingMessage {
    return Cbor.decode<ClientIncomingMessage>(buf);
  }

  private async _isIncomingMessageValid(incomingMessage: ClientIncomingMessage): Promise<boolean> {
    const key = incomingMessage.key;
    const val = new Uint8Array(incomingMessage.val);
    const cert = incomingMessage.cert;
    const tree = incomingMessage.tree;

    // Verify the certificate (canister signature)
    const isValid = await isMessageBodyValid(this.canisterId, key, val, cert, tree, this.agent);

    return isValid;
  }

  private _getContentFromIncomingMessage(incomingMessage: ClientIncomingMessage): ClientIncomingMessageContent {
    const val = new Uint8Array(incomingMessage.val);
    const incomingContent = Cbor.decode<ClientIncomingMessageContent>(val);

    return incomingContent;
  }

  private _isIncomingMessageSequenceNumberValid(incomingContent: ClientIncomingMessageContent): boolean {
    const receivedNum = incomingContent.sequence_num;
    console.log(`Received message sequence number: ${receivedNum}`)
    return receivedNum === this.nextReceivedNum;
  }

  private _inspectIncomingMessageTimestamp(incomingContent: ClientIncomingMessageContent) {
    const time = incomingContent.timestamp;
    const delaySeconds = (Date.now() * (10 ** 6) - time) / (10 ** 9);
    console.log(`(time now) - (message timestamp) = ${delaySeconds}s`);
  }

  private _getApplicationMessageFromIncomingContent(incomingContent: ClientIncomingMessageContent) {
    return Cbor.decode(incomingContent.message);
  }

  private async _makeApplicationMessage(data: any): Promise<CanisterWsMessageArguments> {
    const content = Cbor.encode(data);
    const publicKey = await this._getPublicKey();

    return {
      msg: {
        DirectlyFromClient: {
          client_key: publicKey,
          message: new Uint8Array(content),
        },
      }
    };
  }
}
