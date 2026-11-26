/**
 * Where the user probably is, and how a country is named, for the surfaces
 * that browse by country: Web Radio and the podcast directory.
 */

/** Localized country name for an ISO 3166-1 alpha-2 code, falling back
 *  to the bare code when `Intl.DisplayNames` is unavailable / throws. */
export function countryLabel(uiLang: string, code: string): string {
  try {
    const name = new Intl.DisplayNames([uiLang], { type: "region" }).of(code);
    if (name) return name;
  } catch {
    /* fall through */
  }
  return code;
}

/** IANA time-zone → ISO 3166-1 alpha-2 country. Reflects the user's
 *  physical location far better than the OS UI language (an en-US
 *  Windows install outside the US is extremely common). Not exhaustive
 *  — falls through to the locale region when a zone isn't mapped. */
const TIMEZONE_TO_REGION: Record<string, string> = {
  // Europe
  "Europe/Paris": "FR",
  "Europe/London": "GB",
  "Europe/Dublin": "IE",
  "Europe/Madrid": "ES",
  "Europe/Lisbon": "PT",
  "Europe/Berlin": "DE",
  "Europe/Zurich": "CH",
  "Europe/Vienna": "AT",
  "Europe/Rome": "IT",
  "Europe/Amsterdam": "NL",
  "Europe/Brussels": "BE",
  "Europe/Luxembourg": "LU",
  "Europe/Copenhagen": "DK",
  "Europe/Oslo": "NO",
  "Europe/Stockholm": "SE",
  "Europe/Helsinki": "FI",
  "Europe/Warsaw": "PL",
  "Europe/Prague": "CZ",
  "Europe/Bratislava": "SK",
  "Europe/Budapest": "HU",
  "Europe/Bucharest": "RO",
  "Europe/Sofia": "BG",
  "Europe/Athens": "GR",
  "Europe/Zagreb": "HR",
  "Europe/Belgrade": "RS",
  "Europe/Ljubljana": "SI",
  "Europe/Kyiv": "UA",
  "Europe/Kiev": "UA",
  "Europe/Moscow": "RU",
  "Europe/Istanbul": "TR",
  // Americas
  "America/New_York": "US",
  "America/Chicago": "US",
  "America/Denver": "US",
  "America/Phoenix": "US",
  "America/Los_Angeles": "US",
  "America/Anchorage": "US",
  "Pacific/Honolulu": "US",
  "America/Toronto": "CA",
  "America/Vancouver": "CA",
  "America/Edmonton": "CA",
  "America/Winnipeg": "CA",
  "America/Halifax": "CA",
  "America/Mexico_City": "MX",
  "America/Sao_Paulo": "BR",
  "America/Argentina/Buenos_Aires": "AR",
  "America/Santiago": "CL",
  "America/Bogota": "CO",
  "America/Lima": "PE",
  // Asia / Middle East
  "Asia/Tokyo": "JP",
  "Asia/Seoul": "KR",
  "Asia/Shanghai": "CN",
  "Asia/Hong_Kong": "HK",
  "Asia/Taipei": "TW",
  "Asia/Singapore": "SG",
  "Asia/Bangkok": "TH",
  "Asia/Jakarta": "ID",
  "Asia/Kuala_Lumpur": "MY",
  "Asia/Manila": "PH",
  "Asia/Kolkata": "IN",
  "Asia/Calcutta": "IN",
  "Asia/Dubai": "AE",
  "Asia/Riyadh": "SA",
  "Asia/Jerusalem": "IL",
  "Asia/Tel_Aviv": "IL",
  "Asia/Tehran": "IR",
  // Oceania / Africa
  "Australia/Sydney": "AU",
  "Australia/Melbourne": "AU",
  "Australia/Brisbane": "AU",
  "Australia/Perth": "AU",
  "Pacific/Auckland": "NZ",
  "Africa/Johannesburg": "ZA",
  "Africa/Cairo": "EG",
  "Africa/Lagos": "NG",
  "Africa/Casablanca": "MA",
  "Africa/Tunis": "TN",
  "Africa/Algiers": "DZ",
};

/** Best-effort ISO 3166-1 alpha-2 region for the "Local stations"
 *  default. Prefers the IANA time zone (reflects physical location)
 *  and falls back to the browser locale region ("fr-FR" → "FR") when
 *  the zone isn't mapped. Returns null when neither yields a region
 *  (e.g. a bare "en"). Web Radio's "Local stations" and the podcast
 *  directory start from it until the user picks a country. */
export function detectLocalRegion(): string | null {
  try {
    const tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
    const fromTz = tz ? TIMEZONE_TO_REGION[tz] : undefined;
    if (fromTz) return fromTz;
  } catch {
    // fall through to locale
  }
  try {
    const region = new Intl.Locale(navigator.language).region;
    return region && /^[A-Za-z]{2}$/.test(region) ? region.toUpperCase() : null;
  } catch {
    return null;
  }
}
