/**
 * The podcast directory's fixed lists: the countries it has a storefront with
 * podcasts in, and the genres it charts. Their names are the app's own: a
 * country through `Intl.DisplayNames`, a genre through
 * `podcasts.directory.genres.<id>`, so both read in the interface's language.
 */

/** Where in the directory the page is, carried on the history entry. Neither
 *  set is the country's chart. */
export interface DirectoryPlace {
  genre?: number | null;
  term?: string | null;
}

/**
 * Every country whose chart had shows when this list was made (2026-11-25, each
 * asked once). A storefront without podcasts answers an empty chart or a 400,
 * so it is left out rather than offered empty. Mirrors no backend list: the
 * backend takes any two letters.
 */
export const PODCAST_STOREFRONTS: readonly string[] = [
  "AD",
  "AE",
  "AF",
  "AG",
  "AI",
  "AL",
  "AM",
  "AO",
  "AR",
  "AT",
  "AU",
  "AZ",
  "BA",
  "BB",
  "BD",
  "BE",
  "BF",
  "BG",
  "BH",
  "BJ",
  "BM",
  "BN",
  "BO",
  "BR",
  "BS",
  "BT",
  "BW",
  "BY",
  "BZ",
  "CA",
  "CD",
  "CG",
  "CH",
  "CI",
  "CL",
  "CM",
  "CN",
  "CO",
  "CR",
  "CV",
  "CY",
  "CZ",
  "DE",
  "DK",
  "DM",
  "DO",
  "DZ",
  "EC",
  "EE",
  "EG",
  "ES",
  "ET",
  "FI",
  "FJ",
  "FM",
  "FR",
  "GA",
  "GB",
  "GD",
  "GE",
  "GH",
  "GM",
  "GN",
  "GR",
  "GT",
  "GW",
  "GY",
  "HK",
  "HN",
  "HR",
  "HU",
  "ID",
  "IE",
  "IL",
  "IN",
  "IQ",
  "IS",
  "IT",
  "JM",
  "JO",
  "JP",
  "KE",
  "KG",
  "KH",
  "KN",
  "KR",
  "KW",
  "KY",
  "KZ",
  "LA",
  "LB",
  "LC",
  "LK",
  "LR",
  "LT",
  "LU",
  "LV",
  "LY",
  "MA",
  "MD",
  "ME",
  "MG",
  "MK",
  "ML",
  "MM",
  "MN",
  "MO",
  "MR",
  "MS",
  "MT",
  "MU",
  "MV",
  "MW",
  "MX",
  "MY",
  "MZ",
  "NA",
  "NE",
  "NG",
  "NI",
  "NL",
  "NO",
  "NP",
  "NR",
  "NZ",
  "OM",
  "PA",
  "PE",
  "PG",
  "PH",
  "PK",
  "PL",
  "PT",
  "PW",
  "PY",
  "QA",
  "RO",
  "RS",
  "RU",
  "RW",
  "SA",
  "SB",
  "SC",
  "SE",
  "SG",
  "SI",
  "SK",
  "SL",
  "SN",
  "SR",
  "ST",
  "SV",
  "SZ",
  "TC",
  "TD",
  "TH",
  "TJ",
  "TM",
  "TN",
  "TO",
  "TR",
  "TT",
  "TW",
  "TZ",
  "UA",
  "UG",
  "US",
  "UY",
  "UZ",
  "VC",
  "VE",
  "VG",
  "VN",
  "VU",
  "YE",
  "ZA",
  "ZM",
  "ZW",
];

const STOREFRONTS = new Set(PODCAST_STOREFRONTS);

/** Whether `code` (two upper-case letters) is one of {@link PODCAST_STOREFRONTS}. */
export function isPodcastStorefront(code: string): boolean {
  return STOREFRONTS.has(code);
}

/** The genres the directory charts, in the order the page offers them. Mirrors
 *  `waveflow_core::podcast::directory::GENRES`. */
export const PODCAST_GENRES: readonly number[] = [
  1489, // News
  1324, // Society & Culture
  1488, // True Crime
  1303, // Comedy
  1487, // History
  1533, // Science
  1318, // Technology
  1321, // Business
  1512, // Health & Fitness
  1304, // Education
  1545, // Sports
  1310, // Music
  1309, // TV & Film
  1301, // Arts
  1483, // Fiction
  1502, // Leisure
  1305, // Kids & Family
  1314, // Religion & Spirituality
  1511, // Government
];
