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

export interface Message {
  id: string;
  text: string;
  outgoing: boolean;
  at: number;
}

export interface Network {
  peer_id: string;
  listeners: string[];
  connected: number;
}

export const api = {
  me: () => invoke<Me>("me"),
  myInvite: (displayName: string) => invoke<string>("my_invite", { displayName }),
  addContact: (invite: string) => invoke<Contact>("add_contact", { invite }),
  listContacts: () => invoke<Contact[]>("list_contacts"),
  sendMessage: (to: string, text: string) => invoke<void>("send_message", { to, text }),
  conversation: (that: string) => invoke<Message[]>("conversation", { with: that }),
  network: () => invoke<Network>("network"),
};

/** Fires when a message has been decrypted and stored. Payload: conversation id. */
export const onMessage = (fn: (conversation: string) => void) =>
  listen<string>("vega://message", (e) => fn(e.payload));

/** Fires when the set of reachable peers changes. Payload: peer count. */
export const onNetwork = (fn: (connected: number) => void) =>
  listen<number>("vega://network", (e) => fn(e.payload));
