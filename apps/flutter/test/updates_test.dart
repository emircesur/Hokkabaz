import 'package:flutter_test/flutter_test.dart';
import 'package:hokkabaz/screens/settings/updates.dart';

void main() {
  test('version parsing and ordering', () {
    expect(parseVersion('v1.2.3'), [1, 2, 3]);
    expect(parseVersion('1.2.3-rc1'), [1, 2, 3]);
    expect(parseVersion('nightly'), isNull);
    expect(isNewer('v1.0.1', '1.0.0'), isTrue);
    expect(isNewer('1.0.0', '1.0.0'), isFalse);
    expect(isNewer('1.0', '1.0.0'), isFalse);
    expect(isNewer('2.0.0', '1.9.9'), isTrue);
    expect(isNewer('0.9.0', '1.0.0'), isFalse);
    expect(isNewer('garbage', '1.0.0'), isFalse);
  });
}
