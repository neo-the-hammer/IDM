/**
 * Translations and text direction.
 *
 * Persian is a first-class locale rather than an afterthought: switching to it
 * flips the whole interface to right-to-left, and the layout uses CSS logical
 * properties so that actually works instead of merely mirroring.
 */

export type Locale = 'en' | 'fa';

type Strings = Record<string, string>;

const EN: Strings = {
  'action.add': 'Add download',
  'action.cancel': 'Cancel',
  'action.close': 'Close',
  'action.save': 'Save',
  'action.pause': 'Pause',
  'action.resume': 'Resume',
  'action.retry': 'Retry',
  'action.remove': 'Remove',
  'action.restart': 'Start over',
  'action.openFolder': 'Show in folder',
  'action.copyUrl': 'Copy link',
  'action.pauseAll': 'Pause all',
  'action.resumeAll': 'Resume all',
  'action.clearCompleted': 'Clear completed',
  'action.settings': 'Settings',
  'action.details': 'Details',

  'search.placeholder': 'Search downloads…',

  'nav.status': 'Status',
  'nav.all': 'All downloads',
  'nav.downloading': 'Downloading',
  'nav.queued': 'Queued',
  'nav.paused': 'Paused',
  'nav.completed': 'Completed',
  'nav.failed': 'Failed',
  'nav.categories': 'Categories',

  'status.connecting': 'Connecting…',
  'status.connected': 'Connected',
  'status.offline': 'Daemon unreachable',
  'status.downloading': 'Downloading',
  'status.queued': 'Queued',
  'status.paused': 'Paused',
  'status.completed': 'Completed',
  'status.failed': 'Failed',
  'status.cancelled': 'Cancelled',
  'status.probing': 'Checking…',
  'status.verifying': 'Verifying…',

  'empty.title': 'Nothing here yet',
  'empty.body': 'Add a link to get started, or install the browser extension to capture downloads automatically.',
  'empty.filtered': 'No downloads match this filter.',

  'add.title': 'Add download',
  'add.url': 'URL',
  'add.filename': 'Save as',
  'add.filenameAuto': 'From the server',
  'add.directory': 'Folder',
  'add.directoryAuto': 'By category',
  'add.connections': 'Connections',
  'add.advanced': 'Advanced',
  'add.limit': 'Speed limit',
  'add.checksum': 'Checksum',
  'add.username': 'Username',
  'add.password': 'Password',
  'add.referer': 'Referer',
  'add.paused': 'Add without starting',

  'settings.title': 'Settings',
  'settings.theme': 'Theme',
  'settings.accent': 'Accent colour',
  'settings.accentHint': "Overrides the theme's own accent.",
  'settings.language': 'Language',
  'settings.connections': 'Default connections',
  'settings.speedLimit': 'Global speed limit',
  'settings.maxConcurrent': 'Simultaneous downloads',
  'settings.notifications': 'Notify me when a download finishes',
  'settings.saved': 'Settings saved',

  'detail.url': 'URL',
  'detail.savedTo': 'Saved to',
  'detail.size': 'Size',
  'detail.progress': 'Progress',
  'detail.speed': 'Speed',
  'detail.eta': 'Time left',
  'detail.connections': 'Connections',
  'detail.added': 'Added',
  'detail.finished': 'Finished',
  'detail.type': 'Type',
  'detail.error': 'Error',
  'detail.segments': 'Segments',

  'toast.added': 'Download added',
  'toast.removed': 'Download removed',
  'toast.copied': 'Link copied',
  'toast.cleared': 'Cleared {n} completed downloads',
  'toast.finished': '{name} finished',

  'unit.of': 'of',
};

const FA: Strings = {
  'action.add': 'افزودن دانلود',
  'action.cancel': 'انصراف',
  'action.close': 'بستن',
  'action.save': 'ذخیره',
  'action.pause': 'توقف',
  'action.resume': 'ادامه',
  'action.retry': 'تلاش دوباره',
  'action.remove': 'حذف',
  'action.restart': 'شروع از نو',
  'action.openFolder': 'نمایش در پوشه',
  'action.copyUrl': 'کپی پیوند',
  'action.pauseAll': 'توقف همه',
  'action.resumeAll': 'ادامه همه',
  'action.clearCompleted': 'پاک کردن تمام‌شده‌ها',
  'action.settings': 'تنظیمات',
  'action.details': 'جزئیات',

  'search.placeholder': 'جستجو در دانلودها…',

  'nav.status': 'وضعیت',
  'nav.all': 'همه دانلودها',
  'nav.downloading': 'در حال دانلود',
  'nav.queued': 'در صف',
  'nav.paused': 'متوقف‌شده',
  'nav.completed': 'تمام‌شده',
  'nav.failed': 'ناموفق',
  'nav.categories': 'دسته‌بندی‌ها',

  'status.connecting': 'در حال اتصال…',
  'status.connected': 'متصل',
  'status.offline': 'سرویس در دسترس نیست',
  'status.downloading': 'در حال دانلود',
  'status.queued': 'در صف',
  'status.paused': 'متوقف‌شده',
  'status.completed': 'تمام‌شده',
  'status.failed': 'ناموفق',
  'status.cancelled': 'لغو شده',
  'status.probing': 'در حال بررسی…',
  'status.verifying': 'در حال بررسی صحت…',

  'empty.title': 'هنوز چیزی اینجا نیست',
  'empty.body': 'یک پیوند اضافه کنید، یا افزونه مرورگر را نصب کنید تا دانلودها خودکار گرفته شوند.',
  'empty.filtered': 'هیچ دانلودی با این فیلتر مطابقت ندارد.',

  'add.title': 'افزودن دانلود',
  'add.url': 'نشانی',
  'add.filename': 'ذخیره با نام',
  'add.filenameAuto': 'از سرور',
  'add.directory': 'پوشه',
  'add.directoryAuto': 'بر اساس دسته‌بندی',
  'add.connections': 'تعداد اتصال',
  'add.advanced': 'پیشرفته',
  'add.limit': 'محدودیت سرعت',
  'add.checksum': 'چک‌سام',
  'add.username': 'نام کاربری',
  'add.password': 'گذرواژه',
  'add.referer': 'ارجاع‌دهنده',
  'add.paused': 'افزودن بدون شروع',

  'settings.title': 'تنظیمات',
  'settings.theme': 'پوسته',
  'settings.accent': 'رنگ تأکیدی',
  'settings.accentHint': 'جایگزین رنگ پیش‌فرض پوسته می‌شود.',
  'settings.language': 'زبان',
  'settings.connections': 'اتصال‌های پیش‌فرض',
  'settings.speedLimit': 'محدودیت سرعت کلی',
  'settings.maxConcurrent': 'دانلودهای همزمان',
  'settings.notifications': 'وقتی دانلودی تمام شد اطلاع بده',
  'settings.saved': 'تنظیمات ذخیره شد',

  'detail.url': 'نشانی',
  'detail.savedTo': 'ذخیره در',
  'detail.size': 'حجم',
  'detail.progress': 'پیشرفت',
  'detail.speed': 'سرعت',
  'detail.eta': 'زمان باقی‌مانده',
  'detail.connections': 'اتصال‌ها',
  'detail.added': 'افزوده شده',
  'detail.finished': 'پایان',
  'detail.type': 'نوع',
  'detail.error': 'خطا',
  'detail.segments': 'بخش‌ها',

  'toast.added': 'دانلود افزوده شد',
  'toast.removed': 'دانلود حذف شد',
  'toast.copied': 'پیوند کپی شد',
  'toast.cleared': '{n} دانلود تمام‌شده پاک شد',
  'toast.finished': '{name} تمام شد',

  'unit.of': 'از',
};

const CATALOGUE: Record<Locale, Strings> = { en: EN, fa: FA };

/** Locales whose script runs right to left. */
const RTL: ReadonlySet<Locale> = new Set<Locale>(['fa']);

let current: Locale = 'en';

export function setLocale(locale: Locale): void {
  current = CATALOGUE[locale] ? locale : 'en';
  document.documentElement.lang = current;
  const dir = RTL.has(current) ? 'rtl' : 'ltr';
  document.documentElement.dir = dir;
  document.body.dir = dir;
  applyStaticStrings();
}

export function locale(): Locale {
  return current;
}

export function isRtl(): boolean {
  return RTL.has(current);
}

/** Looks up a key, falling back to English and then to the key itself. */
export function t(key: string, vars?: Record<string, string | number>): string {
  const table = CATALOGUE[current] ?? EN;
  let text = table[key] ?? EN[key] ?? key;
  if (vars) {
    for (const [name, value] of Object.entries(vars)) {
      text = text.replaceAll(`{${name}}`, String(value));
    }
  }
  return text;
}

/** Translates elements marked up in the HTML. */
export function applyStaticStrings(root: ParentNode = document): void {
  root.querySelectorAll<HTMLElement>('[data-i18n]').forEach((element) => {
    const key = element.dataset.i18n;
    if (key) element.textContent = t(key);
  });
  root.querySelectorAll<HTMLElement>('[data-i18n-placeholder]').forEach((element) => {
    const key = element.dataset.i18nPlaceholder;
    if (key && 'placeholder' in element) {
      (element as HTMLInputElement).placeholder = t(key);
    }
  });
  root.querySelectorAll<HTMLElement>('[data-i18n-title]').forEach((element) => {
    const key = element.dataset.i18nTitle;
    if (key) element.title = t(key);
  });
}
