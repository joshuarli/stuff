import unittest

from mycrates.errors import InputError
from mycrates.requirements import canonical_requirement
from mycrates.semver import Version, compare_text


class VersionTest(unittest.TestCase):
    def test_precedence_follows_semver(self):
        ordered = ["0.9.0", "1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta", "1.0.0-beta.2",
                   "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0", "1.0.1", "1.10.0", "2.0.0", "10.0.0"]
        for lower, higher in zip(ordered, ordered[1:]):
            self.assertEqual(compare_text(lower, higher), -1, (lower, higher))
            self.assertEqual(compare_text(higher, lower), 1, (higher, lower))

    def test_build_metadata_is_kept_in_the_name_but_not_in_precedence_or_identity(self):
        a, b = Version.parse("1.0.0+one"), Version.parse("1.0.0+two")
        self.assertEqual(compare_text("1.0.0+one", "1.0.0+two"), 0)
        self.assertNotEqual(str(a), str(b))
        self.assertEqual(a.uniqueness_key(), b.uniqueness_key())
        self.assertEqual(str(Version.parse("1.2.3-rc.1+b.5")), "1.2.3-rc.1+b.5")

    def test_malformed_versions_are_rejected(self):
        for text in ["", "1", "1.0", "1.0.0.0", "01.0.0", "1.0.0-", "1.0.0+", "1.0.0-01", "1.0.0-a..b", "a.b.c",
                     "1.0.0+a_b", "1.0.0 ", "+1.0.0", "18446744073709551616.0.0", "1." * 200 + "0"]:
            with self.subTest(text=text), self.assertRaises(InputError):
                Version.parse(text)

    def test_unparseable_text_still_orders_deterministically(self):
        self.assertEqual(compare_text("1.0.0", "garbage"), 1 if "garbage" < "1.0.0" else -1)
        self.assertEqual(compare_text("b", "a"), 1)


class RequirementTest(unittest.TestCase):
    def same(self, a, b):
        self.assertEqual(canonical_requirement(a), canonical_requirement(b), (a, b))

    def different(self, a, b):
        self.assertNotEqual(canonical_requirement(a), canonical_requirement(b), (a, b))

    def test_equivalent_spellings_agree(self):
        for a, b in [
            ("1", "^1"), ("1.0", "^1.0.0"), ("^1.2", ">=1.2.0, <2.0.0"), ("1.2.*", "~1.2"), ("*", " * "),
            ("^0.0", ">=0.0.0, <0.1.0"), ("^0.2.3", ">=0.2.3, <0.3.0"), ("=1.2.3", "=1.2.3"),
            (">=1,<2", "^1"), ("~1", "^1"), ("1.*", "^1"), (">1.2", ">=1.3.0"), ("<=1", "<2.0.0"),
        ]:
            with self.subTest(a=a, b=b):
                self.same(a, b)

    def test_distinct_requirements_stay_distinct(self):
        for a, b in [("^1", "^2"), ("^0.1", "^0.2"), ("^0.0.1", "^0.0.2"), ("1.2.3", "=1.2.3"), ("~1.2.3", "^1.2.3"),
                     (">=1.0.0", ">1.0.0"), ("^1.2.3-alpha", "^1.2.3")]:
            with self.subTest(a=a, b=b):
                self.different(a, b)

    def test_unsatisfiable_ranges_compare_equal_to_each_other_only(self):
        self.same(">2, <1", ">5, <3")
        self.different(">2, <1", "^1")

    def test_malformed_requirements_are_rejected(self):
        for text in ["", "  ", ",", "^", "1.x.2", "1.2.3.4", "^1,", "~", "=a", "01.2", "1.2.3-"]:
            with self.subTest(text=text), self.assertRaises(InputError):
                canonical_requirement(text)


if __name__ == "__main__":
    unittest.main()
