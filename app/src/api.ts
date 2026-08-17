/** The Rust side, typed. Every call here maps to a `#[tauri::command]`. */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface Me {
  account_id: string;
  short_id: string;
  device_label: string;
  device_id: string;
  /**
   * Your own fingerprint as ten words, to read out while handing an invite
   * over. Fifty-two characters of base32 is not something anybody says down a
   * phone line, and an id nobody checks is an id anybody can swap.
   */
  identity_words: string;
}

export interface Contact {
  account_id: string;
  short_id: string;
  display_name: string;
  verified: boolean;
  safety_number: string;
  /** The same fingerprint as words, for reading aloud instead of the digits. */
  safety_words: string;
  /**
   * Their own phrase, independent of yours — the one they read out when they
   * sent the invite, kept so it can still be compared after the fact.
   */
  identity_words: string;
  /** Messages that arrived since this conversation was last on screen. */
  unread: number;
}

/** What an invite claims to be, before anything has been saved. */
export interface InvitePreview {
  account_id: string;
  short_id: string;
  display_name: string;
  /**
   * Computed from the invite that actually arrived. Read it back to whoever
   * sent it: a phrase that differs from theirs means it was swapped in transit.
   */
  identity_words: string;
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
  /** A thread key: a contact's account id, or `group:<id>`. */
  account_id: string;
  display_name: string;
  /** True when this is a group rather than a contact. */
  group: boolean;
  messages: number;
}

/** One member of a group. */
export interface GroupMember {
  account_id: string;
  display_name: string;
  /** This is you. */
  self_: boolean;
  /**
   * Not one of your contacts, so you cannot send to them and they will not see
   * what you write. Whoever made the group knows them; you do not. Shown rather
   * than hidden — a member list that quietly leaves people out is a lie about
   * who is reading.
   */
  unreachable: boolean;
}

/**
 * A group.
 *
 * `id` is the thread key, in the same form every thread call takes, so a group
 * and a contact are interchangeable everywhere a conversation is named.
 */
export interface Group {
  id: string;
  name: string;
  /** The one account that may change the membership. */
  creator: string;
  /** That creator is you — everyone else can only leave. */
  mine: boolean;
  members: GroupMember[];
  /** You are no longer in it. The thread stays readable and stops sending. */
  departed: boolean;
  unread: number;
}

/**
 * The most people a group holds.
 *
 * Mirrors `MAX_GROUP_MEMBERS` in vega-core, which is the side that enforces it.
 * Every message is sent once per member device, so this is a bound on how much
 * traffic one message can turn into.
 */
export const MAX_GROUP_MEMBERS = 32;

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
  /**
   * Who an invite says it is, without saving it. Verified exactly as
   * `addContact` verifies it, so a preview that comes back is a real invite —
   * this only stops short of keeping it.
   */
  previewInvite: (invite: string) =>
    invoke<InvitePreview>("preview_invite", { invite }),
  addContact: (invite: string) => invoke<Contact>("add_contact", { invite }),
  listContacts: () => invoke<Contact[]>("list_contacts"),
  sendMessage: (to: string, text: string) =>
    invoke<void>("send_message", { to, text }),
  sendFile: (to: string, name: string, data: string) =>
    invoke<void>("send_file", { to, name, data }),
  conversation: (that: string) =>
    invoke<Message[]>("conversation", { with: that }),

  /**
   * Move a conversation's read marker to the end of what is in it. Called when
   * the conversation is on screen — which is the only thing this program can
   * honestly observe about reading — and returns what is left unread.
   */
  markRead: (that: string) => invoke<number>("mark_read", { with: that }),
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

  /**
   * Remember that the safety words were compared, and how it went. The
   * comparison happens on a call or in person; this only records the answer,
   * and it can be taken back.
   */
  setVerified: (account: string, verified: boolean) =>
    invoke<Contact>("set_verified", { account, verified }),

  /**
   * Start a group.
   *
   * Everybody named has to be a contact already: there is no directory to look
   * anyone up in, and a group is not a way around exchanging invites. They are
   * all told immediately, which is also how they learn they are in it — there
   * is no invitation to accept, for the same reason there is no server to hold
   * one.
   */
  createGroup: (name: string, members: string[]) =>
    invoke<Group>("create_group", { name, members }),
  listGroups: () => invoke<Group[]>("list_groups"),
  /** Returns the members it could not be delivered to, if any. */
  sendGroupMessage: (group: string, text: string) =>
    invoke<string[]>("send_group_message", { group, text }),
  /** Creator only. Everyone else is refused by the Rust side. */
  addToGroup: (group: string, account: string) =>
    invoke<Group>("add_to_group", { group, account }),
  removeFromGroup: (group: string, account: string) =>
    invoke<Group>("remove_from_group", { group, account }),
  renameGroup: (group: string, name: string) =>
    invoke<Group>("rename_group", { group, name }),
  /** Tells the others on the way out. The thread stays. */
  leaveGroup: (group: string) => invoke<Group>("leave_group", { group }),
  /** Local. The others keep their copy, and nothing tells them. */
  deleteGroup: (group: string) => invoke<number>("delete_group", { group }),

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
