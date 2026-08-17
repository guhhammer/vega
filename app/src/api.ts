/** The Rust side, typed. Every call here maps to a `#[tauri::command]`. */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface Me {
  account_id: string;
  short_id: string;
  device_label: string;
  device_id: string;
}

export interface Contact {
  account_id: string;
  short_id: string;
  display_name: string;
  verified: boolean;
  safety_number: string;
}

export interface FileInfo {
  name: string;
  size: number;
  /**
   * Whether the whole file has arrived. There is no path to show: what Vega
   * keeps on disk is encrypted, and `exportFile` is how a usable copy is made.
   */
  ready: boolean;
  /**
   * The image type, when the bytes on disk are one Vega will render — decided
   * by Rust from the file's first bytes, never from its name. Null means show
   * it as a plain file.
   */
  image: string | null;
  have: number;
  chunks: number;
  /** Names the file for `readImage`. */
  transfer: string;
}

export interface Message {
  id: string;
  text: string;
  outgoing: boolean;
  at: number;
  /** Set when this message is a file rather than text. */
  file: FileInfo | null;
}

/** One conversation as the history screen lists it. */
export interface History {
  account_id: string;
  display_name: string;
  messages: number;
}

/**
 * The largest file Vega will send.
 *
 * Mirrors `MAX_FILE_BYTES` in vega-core, which is the side that actually
 * enforces it — this copy exists only so the picker can say no immediately
 * rather than after reading ten megabytes it is about to throw away.
 */
export const MAX_FILE_BYTES = 10 * 1024 * 1024;

export interface Network {
  peer_id: string;
  listeners: string[];
  connected: number;
}

export const api = {
  me: () => invoke<Me>("me"),
  myInvite: (displayName: string) =>
    invoke<string>("my_invite", { displayName }),
  addContact: (invite: string) => invoke<Contact>("add_contact", { invite }),
  listContacts: () => invoke<Contact[]>("list_contacts"),
  sendMessage: (to: string, text: string) =>
    invoke<void>("send_message", { to, text }),
  sendFile: (to: string, name: string, data: string) =>
    invoke<void>("send_file", { to, name, data }),
  conversation: (that: string) =>
    invoke<Message[]>("conversation", { with: that }),
  network: () => invoke<Network>("network"),

  /**
   * A received image, as a `data:` URL ready for an `<img src>`.
   *
   * The bytes come through Rust rather than the page reading the file, which is
   * what keeps the app's capability list empty — the web view has no filesystem
   * access at all, and asking for it would be a much larger grant than showing
   * one picture is worth.
   */
  readImage: async (transfer: string) => {
    const img = await invoke<{ mime: string; data: string }>("read_image", {
      transfer,
    });
    return `data:${img.mime};base64,${img.data}`;
  },

  /**
   * Write a decrypted copy into the downloads folder and return where it went.
   *
   * From that moment the copy is an ordinary file with ordinary protection —
   * which is the whole reason it is a deliberate action rather than the default.
   */
  exportFile: (transfer: string, name: string) =>
    invoke<string>("export_file", { transfer, name }),

  /** Local only. The account id identifies them on the wire, and it never changes. */
  renameContact: (account: string, name: string) =>
    invoke<Contact>("rename_contact", { account, name }),
  /** Local only. Nothing on the wire carries this. */
  renameDevice: (name: string) => invoke<string>("rename_device", { name }),

  clearChat: (that: string) => invoke<number>("clear_chat", { with: that }),
  clearAllHistory: () => invoke<number>("clear_all_history"),
  history: () => invoke<History[]>("history"),
};

/**
 * Base64 for the hop into Rust.
 *
 * `btoa(String.fromCharCode(...bytes))` is the one-liner for this and it throws
 * on anything large: the spread passes one argument per byte, and ten megabytes
 * of arguments is far past what a call can take. Walking the array in slices
 * keeps every call small.
 */
export function toBase64(bytes: Uint8Array): string {
  const step = 0x8000;
  let binary = "";
  for (let i = 0; i < bytes.length; i += step) {
    binary += String.fromCharCode(...bytes.subarray(i, i + step));
  }
  return btoa(binary);
}

/** Fires when a message has been decrypted and stored. Payload: conversation id. */
export const onMessage = (fn: (conversation: string) => void) =>
  listen<string>("vega://message", (e) => fn(e.payload));

/** Fires when the set of reachable peers changes. Payload: peer count. */
export const onNetwork = (fn: (connected: number) => void) =>
  listen<number>("vega://network", (e) => fn(e.payload));
