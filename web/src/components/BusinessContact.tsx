import { useTranslation } from "react-i18next";
import { useBranding } from "../api/hooks";

/** Public operator help identity, shared by login and both portal contexts. */
export function BusinessContact() {
  const { t } = useTranslation();
  const { data } = useBranding();
  if (!data?.contactEmail && !data?.helpUrl) return null;
  return (
    <footer className="flex flex-wrap justify-center gap-4 px-4 py-4 text-sm">
      {data.contactEmail ? <a className="break-all text-primary" href={`mailto:${encodeURIComponent(data.contactEmail)}`}>{data.contactEmail}</a> : null}
      {data.helpUrl ? <a className="text-primary" href={data.helpUrl} target="_blank" rel="noopener noreferrer">{t("biz.profile.helpLink")}</a> : null}
    </footer>
  );
}
