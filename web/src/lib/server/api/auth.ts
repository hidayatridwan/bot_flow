import type { ApiClient } from './client';

/**
 * The wire shape. snake_case stops here: everything above this file speaks `SessionUser`. That
 * boundary is why a rename in the Rust API would touch exactly one file.
 */

export interface RegisterBody {
	email: string;
	password: string;
	tenant_name: string;
	slug?: string;
}

export interface RegisterResponse {
	session_token: string;
	tenant_id: string;
	/** The one-time `sk_` reveal. Only its hash is stored; if we drop it, it is gone forever. */
	api_key: string;
	note: string;
}

export interface LoginBody {
	email: string;
	password: string;
}

export interface LoginResponse {
	session_token: string;
	tenant_id: string;
}

export interface MeResponse {
	account: { email: string };
	tenant: { id: string; name: string };
}

export const register = (client: ApiClient, body: RegisterBody) =>
	client.post<RegisterResponse>('/auth/register', body);

export const login = (client: ApiClient, body: LoginBody) =>
	client.post<LoginResponse>('/auth/login', body);

/** 204, empty body. */
export const logout = (client: ApiClient) => client.post<void>('/auth/logout');

export const me = (client: ApiClient) => client.get<MeResponse>('/auth/me');

export interface ForgotPasswordBody {
	email: string;
}

export interface ResetPasswordBody {
	token: string;
	password: string;
}

export interface ChangePasswordBody {
	current_password: string;
	new_password: string;
}

/**
 * 202, empty body — **always**, whether or not the address is registered.
 *
 * Do not add error handling that distinguishes those cases here or above: the API returns one
 * status on purpose so the endpoint cannot be used to discover which emails exist, and the only way
 * to break that is from this side. See `mapForgotPasswordError`.
 */
export const forgotPassword = (client: ApiClient, body: ForgotPasswordBody) =>
	client.post<void>('/auth/password/forgot', body);

/** 204 on success; 400 when the link is expired, already used, or never existed. */
export const resetPassword = (client: ApiClient, body: ResetPasswordBody) =>
	client.post<void>('/auth/password/reset', body);

/** 204 on success; 403 when the current password is wrong (not 401 — see `mapChangePasswordError`). */
export const changePassword = (client: ApiClient, body: ChangePasswordBody) =>
	client.post<void>('/auth/password', body);
