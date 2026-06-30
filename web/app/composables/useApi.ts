// Typed wrappers around the ruskery dashboard API.

export type Role = "owner" | "admin" | "member";

export interface User {
  id: string;
  email: string;
  username: string;
  is_admin: boolean;
  created_at: string;
}
export interface OrgRef {
  id: string;
  slug: string;
  name: string;
  role: Role;
}
export interface Me {
  user: User;
  orgs: OrgRef[];
}
export interface OrgStats {
  repos: number;
  members: number;
  teams: number;
}
export interface RepoSummary {
  name: string;
  tag_count: number;
  updated_at: string;
}
export interface TagDetail {
  tag: string;
  digest: string;
  size: number;
  updated_at: string;
}
export interface Member {
  user_id: string;
  username: string;
  email: string;
  role: string;
}
export interface Team {
  id: string;
  slug: string;
  name: string;
}
export interface TeamPerm {
  repo: string;
  permission: string;
}
export interface Token {
  id: string;
  name: string;
  token_prefix: string;
  last_used_at: string | null;
  created_at: string;
}

export function useApi() {
  const call = <T>(method: string, path: string, body?: unknown) =>
    $fetch<T>(path, {
      method: method as any,
      body: body as any,
      credentials: "same-origin",
    });
  return {
    get: <T>(p: string) => call<T>("GET", p),
    post: <T>(p: string, b?: unknown) => call<T>("POST", p, b),
    del: <T>(p: string) => call<T>("DELETE", p),
  };
}

/** Reactive current-user state, shared across the app. */
export function useMe() {
  return useState<Me | null>("me", () => null);
}

export function apiErrorMessage(e: any): string {
  return (
    e?.data?.error?.message ||
    e?.data?.errors?.[0]?.message ||
    e?.message ||
    "Request failed"
  );
}
