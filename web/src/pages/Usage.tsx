import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useReducedMotion } from "motion/react";
import { ChartColumn } from "lucide-react";
import { useDisplay, useKeys, useUsage } from "../api/hooks";
import {
  Cr,
  EmptyRows,
  PageState,
  Select,
  SURFACE,
  TableShell,
  Td,
  creditsWithDisplay,
  type DisplayCurrency,
} from "../components/ui";

// The usage page: the trailing 30 UTC days, summed per day for the chart and
// listed per (day, channel, model) slice in the table — the same window and
// scope the Topcoat page draws.
const USAGE_DAYS = 30;

function dayString(offset: number): string {
  const d = new Date();
  d.setUTCDate(d.getUTCDate() - offset);
  return d.toISOString().slice(0, 10);
}

export function Usage() {
  const { t } = useTranslation();
  const [keyFilter, setKeyFilter] = useState("");
  const display = useDisplay();
  const keys = useKeys();
  const usage = useUsage(dayString(USAGE_DAYS - 1), dayString(0));

  const rows = useMemo(
    () =>
      (usage.data?.days ?? []).filter(
        (row) => !keyFilter || row.keyId === keyFilter,
      ),
    [usage.data, keyFilter],
  );

  const perDay = useMemo(() => {
    const map = new Map<string, number>();
    for (let i = USAGE_DAYS - 1; i >= 0; i--) {
      map.set(dayString(i), 0);
    }
    for (const row of rows) {
      map.set(row.day, (map.get(row.day) ?? 0) + row.chargedMinor);
    }
    return map;
  }, [rows]);

  const max = Math.max(1, ...perDay.values());

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">{t("usage.title")}</h1>
        <Select value={keyFilter} onChange={(e) => setKeyFilter(e.target.value)} aria-label={t("usage.keyFilter")}>
          <option value="">{t("usage.allKeys")}</option>
          {(keys.data ?? []).map((key) => (
            <option key={key.id} value={key.id}>
              {key.name ?? key.prefix}
            </option>
          ))}
        </Select>
      </div>

      {/* The daily trend: a trend over time reads as a line/area, not 30
          barcode bars (chart-crafting §4). Hand-drawn SVG — one series, no
          chart dependency — with a zero baseline, day ticks and a tooltip. */}
      <UsageChart perDay={perDay} max={max} currency={display} />

      {usage.isError ? (
        <PageState>{t("usage.readError")}</PageState>
      ) : (
        <TableShell
          head={[
            t("usage.day"),
            t("common.channel"),
            t("common.model"),
            t("usage.key"),
            { label: t("common.turns"), align: "right" },
            { label: t("usage.in"), align: "right" },
            { label: t("usage.out"), align: "right" },
            { label: t("usage.cached"), align: "right" },
            { label: t("usage.reasoning"), align: "right" },
            { label: t("usage.charged"), align: "right" },
          ]}
          loading={usage.isLoading ? 8 : 0}
        >
          {!usage.isLoading &&
            rows.map((row, i) => (
              <tr key={`${row.day}-${row.channel}-${row.model}-${row.keyId ?? "shared"}-${i}`}>
                <td>{row.day}</td>
                <td>{row.channel}</td>
                <td>{row.model}</td>
                <td className="mono">{keyLabel(row.keyId, keys.data, t("usage.shared"))}</td>
                <Td mono align="right">{row.turns}</Td>
                <Td mono align="right">{row.inputTokens}</Td>
                <Td mono align="right">{row.outputTokens}</Td>
                <Td mono align="right">{row.cachedTokens}</Td>
                <Td mono align="right">{row.reasoningTokens}</Td>
                <Td mono align="right"><Cr minor={row.chargedMinor} currency={display} /></Td>
              </tr>
            ))}
          {!usage.isLoading && rows.length === 0 ? (
            <EmptyRows colSpan={10} icon={<ChartColumn />}>{t("usage.empty")}</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </div>
  );
}

function keyLabel(keyId: string | null | undefined, keys: { id: string; name?: string | null; prefix: string }[] | undefined, sharedLabel: string) {
  if (!keyId) {
    return sharedLabel;
  }
  const key = keys?.find((k) => k.id === keyId || k.id.replaceAll("-", "") === keyId);
  return key ? (key.name ?? key.prefix) : keyId.slice(0, 8);
}

/**
 * The 30-day charged-amount trend, drawn as an area chart: daily value → an
 * `x` step, `y` anchored to a shared zero baseline. A real tooltip rides the
 * nearest point on hover (not the `title` attribute), x-axis day ticks show
 * every 5th day, and the area draws itself in once — all of it silenced under
 * prefers-reduced-motion.
 */
function UsageChart({
  perDay,
  max,
  currency,
}: {
  perDay: Map<string, number>;
  max: number;
  currency: DisplayCurrency | null;
}) {
  const { t } = useTranslation();
  const reduceMotion = useReducedMotion();
  const [hover, setHover] = useState<number | null>(null);
  const wrapRef = useRef<HTMLDivElement>(null);
  const [wrapW, setWrapW] = useState(720);

  useEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setWrapW(el.clientWidth));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const days = [...perDay.keys()];
  const vals = days.map((d) => perDay.get(d) ?? 0);
  const n = days.length;

  // Plot geometry: a fixed logical box sized to the measured container.
  const W = Math.max(320, wrapW);
  const H = 176;
  const padL = 8;
  const padR = 8;
  const padT = 12;
  const padB = 26; // room for the day ticks
  const innerW = W - padL - padR;
  const innerH = H - padT - padB;
  const x = (i: number) => padL + (n <= 1 ? innerW / 2 : (i / (n - 1)) * innerW);
  const y = (v: number) => padT + innerH - (v / max) * innerH;

  const line = vals.map((v, i) => `${i === 0 ? "M" : "L"}${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(" ");
  const area = `${line} L${x(n - 1).toFixed(1)},${(padT + innerH).toFixed(1)} L${x(0).toFixed(1)},${(padT + innerH).toFixed(1)} Z`;

  // Tick every 5th day, plus the last, so labels stay readable at 30 days.
  const tickEvery = Math.max(1, Math.ceil(n / 6));

  return (
    <div ref={wrapRef} className={`${SURFACE} p-4`}>
      <svg
        viewBox={`0 0 ${W} ${H}`}
        className="block h-44 w-full"
        role="img"
        aria-label={t("usage.dailyCharge",{count:n})}
        onMouseLeave={() => setHover(null)}
        onMouseMove={(e) => {
          const rect = e.currentTarget.getBoundingClientRect();
          const fx = ((e.clientX - rect.left) / rect.width) * W;
          // nearest point index
          const i = Math.round(((fx - padL) / innerW) * (n - 1));
          setHover(Math.max(0, Math.min(n - 1, i)));
        }}
      >
        <defs>
          <linearGradient id="usageFill" x1="0" x2="0" y1="0" y2="1">
            <stop offset="0%" stopColor="var(--color-primary)" stopOpacity="0.28" />
            <stop offset="100%" stopColor="var(--color-primary)" stopOpacity="0.02" />
          </linearGradient>
        </defs>

        {/* zero baseline */}
        <line x1={padL} x2={W - padR} y1={padT + innerH} y2={padT + innerH} stroke="var(--color-line)" />

        {/* area fill — fades in, silenced under reduced-motion */}
        <path d={area} fill="url(#usageFill)">
          {!reduceMotion ? <animate attributeName="opacity" from="0" to="1" dur="0.7s" fill="freeze" /> : null}
        </path>

        {/* the line — draws itself on once */}
        <path
          d={line}
          fill="none"
          stroke="var(--color-primary)"
          strokeWidth="2"
          strokeLinecap="round"
          pathLength={reduceMotion ? undefined : 1}
          style={
            reduceMotion
              ? undefined
              : { strokeDasharray: 1, strokeDashoffset: 1, animation: "usage-draw 0.9s ease-out forwards" }
          }
        />

        {/* day ticks + value marker on hover */}
        {days.map((d, i) =>
          i % tickEvery === 0 || i === n - 1 ? (
            <text
              key={d}
              x={x(i)}
              y={H - 8}
              textAnchor="middle"
              className="fill-muted"
              style={{ fontSize: 10 }}
            >
              {d.slice(5)}
            </text>
          ) : null,
        )}

        {hover !== null ? (
          <g>
            <line
              x1={x(hover)}
              x2={x(hover)}
              y1={padT}
              y2={padT + innerH}
              stroke="var(--color-muted)"
              strokeDasharray="3 3"
              strokeOpacity={0.5}
            />
            <circle cx={x(hover)} cy={y(vals[hover])} r={3.5} fill="var(--color-primary)" />
          </g>
        ) : null}
      </svg>

      {/* the tooltip — a real element, positioned over the chart */}
      <div className="mt-1 h-4 text-center text-xs text-muted" aria-live="polite">
        {hover !== null ? `${days[hover]} · ${creditsWithDisplay(vals[hover], currency)}` : t("usage.lastNDays",{count:n})}
      </div>

      {!reduceMotion ? (
        <style>{`@keyframes usage-draw { to { stroke-dashoffset: 0 } }`}</style>
      ) : null}
    </div>
  );
}
