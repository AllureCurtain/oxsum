// Component vocabulary barrel — `import { X } from "../components/ui"` keeps
// working while the implementations live one file per domain.

export * from "./money";
export * from "./surface";
export * from "./state";
export * from "./table";
export * from "./badge";
export * from "./form";
export * from "./controls";
export * from "./tabs";
export * from "./select";
export * from "./switch";
export * from "./progress";
export * from "./dialog";
export * from "./alert-dialog";
export * from "./dropdown-menu";
export * from "./popover";
export * from "./tooltip";
export * from "./sonner";
export * from "./skeleton";
export * from "./sheet";
export * from "./hover-card";
export * from "./separator";
export * from "./scroll-area";
export * from "./label";
export * from "./checkbox";
export * from "./radio-group";
export * from "./breadcrumb";
export * from "./pagination";
export * from "./avatar";
export * from "./toggle";
// `card.tsx` exports `Card` alongside CardHeader/CardTitle/… — it stays out of
// the barrel while `surface.tsx`'s business `Card` (title/actions props)
// holds the name. The page-adoption pass picks which `Card` wins.
