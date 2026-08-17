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
  /** Where it is on disk, once all of it is. Null while it is still arriving. */
  path: string | null;
  have: number;
  chunks: number;
}

export interface Message {
  id: string;
  text: string;
  outgoing: boolean;
  at: number;
  /** Set when this message is a file rather than text. */
  file: FileInfo | null;
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
