import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../client";
import type { components } from "../schema";
export type NotificationEndpoint = components["schemas"]["ApplicationNotificationEndpoint"];
export type NotificationRequest = components["schemas"]["ApplicationNotificationEndpointRequest"];
export type NotificationMutation = components["schemas"]["ApplicationNotificationMutation"];
export type NotificationEvent = components["schemas"]["ApplicationNotificationEvent"];
const root = (app: string) => `/business/applications/${app}/notification-endpoints`;
export function useNotificationEndpoints(app: string) {
  return useQuery({ queryKey: ["app-notifications", app], queryFn: () => api<components["schemas"]["ApplicationNotificationEndpoints"]>(root(app)) });
}
export function useCreateNotification(app: string) {
  const client = useQueryClient();
  return useMutation({ mutationFn: (body: NotificationRequest) => api<components["schemas"]["CreatedApplicationNotificationEndpoint"]>(root(app), { method: "POST", body }), onSuccess: () => client.invalidateQueries({ queryKey: ["app-notifications", app] }) });
}
export function useDisableNotification(app: string, endpoint: string) {
  const client = useQueryClient();
  return useMutation({ mutationFn: (body: NotificationMutation) => api<NotificationEndpoint>(`${root(app)}/${endpoint}/disable`, { method: "POST", body }), onSuccess: () => client.invalidateQueries({ queryKey: ["app-notifications", app] }) });
}
export function useNotificationDeliveries(app: string, endpoint: string, after?: string) {
  return useQuery({ queryKey: ["app-notification-deliveries", app, endpoint, after ?? null], queryFn: () => api<components["schemas"]["ApplicationNotificationDeliveries"]>(`${root(app)}/${endpoint}/deliveries?limit=20${after ? `&after=${encodeURIComponent(after)}` : ""}`) });
}
export function useRetryNotification(app: string, endpoint: string) {
  const client = useQueryClient();
  return useMutation({ mutationFn: ({ delivery, ...body }: NotificationMutation & { delivery: string }) => api<components["schemas"]["ApplicationNotificationDelivery"]>(`${root(app)}/${endpoint}/deliveries/${delivery}/retry`, { method: "POST", body }), onSuccess: () => client.invalidateQueries({ queryKey: ["app-notification-deliveries", app, endpoint] }) });
}
