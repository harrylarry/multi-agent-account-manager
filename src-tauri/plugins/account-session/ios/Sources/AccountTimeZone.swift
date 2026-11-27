import Foundation
import WebKit

enum AccountTimeZone {
  static func install(_ timezone: String?, into config: WKWebViewConfiguration) {
    guard let timezone, TimeZone(identifier: timezone) != nil,
          let encoded = try? JSONSerialization.data(withJSONObject: [timezone]),
          let array = String(data: encoded, encoding: .utf8) else { return }
    let source = "const maamAccountTimeZone = \(array)[0];\n" + script
    config.userContentController.addUserScript(
      WKUserScript(source: source, injectionTime: .atDocumentStart, forMainFrameOnly: true)
    )
  }

  private static let script = #"""
  (() => {
    const NativeDateTimeFormat = Intl.DateTimeFormat;
    const optionsWithZone = (options) => {
      if (options == null) return { timeZone: maamAccountTimeZone };
      if (options.timeZone !== undefined) return options;
      return Object.assign(Object.create(Object(options)), { timeZone: maamAccountTimeZone });
    };
    const wrapped = new Proxy(NativeDateTimeFormat, {
      apply(target, receiver, [locales, options]) {
        return Reflect.apply(target, receiver, [locales, optionsWithZone(options)]);
      },
      construct(target, [locales, options], receiver) {
        return Reflect.construct(target, [locales, optionsWithZone(options)], receiver);
      },
    });
    Object.defineProperty(Intl, "DateTimeFormat", {
      configurable: true, writable: true, value: wrapped,
    });
    for (const name of ["toLocaleString", "toLocaleDateString", "toLocaleTimeString"]) {
      const format = Date.prototype[name];
      Object.defineProperty(Date.prototype, name, {
        configurable: true, writable: true,
        value: function(locales, options) {
          return format.call(this, locales, optionsWithZone(options));
        },
      });
    }
  })();
  """#
}
