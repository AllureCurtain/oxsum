import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../client";
import type { components } from "../schema";

export type PaymentOrder = components["schemas"]["PaymentOrder"];
export type OrderCheckout = components["schemas"]["OrderCheckout"];
export type OrderCheckoutSession = components["schemas"]["OrderCheckoutSession"];
export type OrderFulfillment = components["schemas"]["OrderFulfillment"];
export type OrderPurpose = components["schemas"]["OrderPurpose"];
export type MerchantRail = components["schemas"]["MerchantRail"];
type PaymentOrderPage = components["schemas"]["PaymentOrderPage"];

export interface OrderListQuery {
  purpose?: OrderPurpose;
  status?: string;
  after?: string;
}

/** The customer's order history (issue #607): newest first, `purpose`/`status`
 * narrow the page. */
export function useOrders(query?: OrderListQuery) {
  const params = new URLSearchParams();
  if (query?.purpose) params.set("purpose", query.purpose);
  if (query?.status) params.set("status", query.status);
  if (query?.after) params.set("after", query.after);
  const qs = params.toString();
  return useQuery({
    queryKey: ["orders", qs],
    queryFn: () => api<PaymentOrderPage>(`/orders${qs ? `?${qs}` : ""}`),
  });
}

export function useOrder(orderId: string | undefined) {
  return useQuery({
    queryKey: ["order", orderId],
    queryFn: () => api<PaymentOrder>(`/orders/${orderId}`),
    enabled: Boolean(orderId),
    refetchInterval: (query) => {
      const status = query.state.data?.status;
      return status === "created" || status === "provider_pending" || status === "paid" ? 2_000 : false;
    },
  });
}

/** The live checkout session the detail page polls (issue #607): the stored
 * provider handles plus the order's lifecycle truth — a `provider_pending`
 * order mounts `clientSecret`/`sdkUrl`, `paid`/`fulfilled`/`failed`/`expired`
 * answer their truth. Polls while the order can still move. */
export function useCheckoutSession(orderId: string | undefined, options?: { refetchInterval?: number | false }) {
  return useQuery({
    queryKey: ["checkout-session", orderId],
    queryFn: () => api<OrderCheckoutSession>(`/orders/${orderId}/checkout-session`),
    enabled: Boolean(orderId),
    refetchInterval: options?.refetchInterval,
  });
}

export function useOrderFulfillment(orderId: string | undefined, options?: { refetchInterval?: number | false }) {
  return useQuery({
    queryKey: ["order-fulfillment", orderId],
    queryFn: () => api<OrderFulfillment>(`/orders/${orderId}/fulfillment`),
    enabled: Boolean(orderId),
    refetchInterval: options?.refetchInterval,
  });
}

export type CreateOrderBody = Record<string, unknown> & {
  idempotencyKey: string;
  rail: MerchantRail;
  purpose: OrderPurpose;
  displayAmountMinor: number;
  displayCurrency: string;
  promoMinor?: number;
};

export function useCreateOrder() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: CreateOrderBody) =>
      api<OrderCheckout>("/orders", { method: "POST", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["orders"] }),
  });
}

// ---- refunds (issue #608, T17): the customer's ask for review — never a
// self-grant. `useOrderRefunds` reads the order's refund history plus the
// refundable remainder; `useRequestRefund` records the ask under an
// idempotency key a replay answers the standing refund under.

export type OrderRefund = components["schemas"]["OrderRefund"];
export type OrderRefundList = components["schemas"]["OrderRefundList"];

/** `GET /api/v1/orders/{id}/refunds`: the refunds on one order plus
 * `refundableMinor` — what may still be asked back. */
export function useOrderRefunds(orderId: string | undefined) {
  return useQuery({
    queryKey: ["order-refunds", orderId],
    queryFn: () => api<OrderRefundList>(`/orders/${orderId}/refunds`),
    enabled: Boolean(orderId),
  });
}

/** `POST /api/v1/orders/{id}/refund-request`: the recorded `requested` refund —
 * a review record staff resolves; it never grants money itself. `amountMinor`
 * absent asks for the whole refundable remainder. */
export function useRequestRefund(orderId: string | undefined) {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: { reason: string; amountMinor?: number | null }) =>
      api<OrderRefund>(`/orders/${orderId}/refund-request`, {
        method: "POST",
        body: { idempotencyKey: crypto.randomUUID(), ...body },
      }),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["order-refunds", orderId] });
      client.invalidateQueries({ queryKey: ["order", orderId] });
    },
  });
}
