//! Vanilla's default block tags that placement reads, resolved from the built-in data pack
//! (`data/minecraft/tags/block`). Data-pack overrides of these tags are not applied.
//!
//! Generated from the Vanilla tag files; regenerate rather than edit by hand.

use vanillite_data::block::{BlockId, BlockState};

/// A resolved block tag: sorted block ids.
#[derive(Debug)]
pub struct Tag(&'static [u16]);

impl Tag {
    /// Whether `block` is in the tag.
    #[inline]
    pub fn contains(&self, block: BlockId) -> bool {
        self.0.binary_search(&block.0).is_ok()
    }

    /// Whether the block of `state` is in the tag (`BlockState.is(TagKey)`).
    #[inline]
    pub fn has(&self, state: BlockState) -> bool {
        self.contains(state.block())
    }
}

/// `#minecraft:all_hanging_signs`.
pub static ALL_HANGING_SIGNS: Tag = Tag(&[
    280, 281, 282, 283, 284, 285, 286, 287, 288, 289, 290, 291, 292, 293, 294, 295, 296, 297, 298,
    299, 300, 301, 302, 303, 304, 305,
]);
/// `#minecraft:banners`.
pub static BANNERS: Tag = Tag(&[
    617, 618, 619, 620, 621, 622, 623, 624, 625, 626, 627, 628, 629, 630, 631, 632, 633, 634, 635,
    636, 637, 638, 639, 640, 641, 642, 643, 644, 645, 646, 647, 648,
]);
/// `#minecraft:bars`.
pub static BARS: Tag = Tag(&[391, 392, 393, 394, 395, 396, 397, 398, 399]);
/// `#minecraft:campfires`.
pub static CAMPFIRES: Tag = Tag(&[949, 950]);
/// `#minecraft:candle_cakes`.
pub static CANDLE_CAKES: Tag = Tag(&[
    1051, 1052, 1053, 1054, 1055, 1056, 1057, 1058, 1059, 1060, 1061, 1062, 1063, 1064, 1065, 1066,
    1067,
]);
/// `#minecraft:candles`.
pub static CANDLES: Tag = Tag(&[
    1034, 1035, 1036, 1037, 1038, 1039, 1040, 1041, 1042, 1043, 1044, 1045, 1046, 1047, 1048, 1049,
    1050,
]);
/// `#minecraft:cannot_support_kelp`.
pub static CANNOT_SUPPORT_KELP: Tag = Tag(&[729]);
/// `#minecraft:cannot_support_seagrass`.
pub static CANNOT_SUPPORT_SEAGRASS: Tag = Tag(&[729]);
/// `#minecraft:cannot_support_snow_layer`.
pub static CANNOT_SUPPORT_SNOW_LAYER: Tag = Tag(&[326, 578, 610]);
/// `#minecraft:causes_continuous_geyser_eruptions`.
pub static CAUSES_CONTINUOUS_GEYSER_ERUPTIONS: Tag = Tag(&[38]);
/// `#minecraft:causes_periodic_geyser_eruptions`.
pub static CAUSES_PERIODIC_GEYSER_ERUPTIONS: Tag = Tag(&[729]);
/// `#minecraft:convertible_to_mud`.
pub static CONVERTIBLE_TO_MUD: Tag = Tag(&[9, 10, 1239]);
/// `#minecraft:copper`.
pub static COPPER: Tag = Tag(&[1124, 1125, 1126, 1127, 1128, 1129, 1130, 1131]);
/// `#minecraft:copper_chests`.
pub static COPPER_CHESTS: Tag = Tag(&[1198, 1199, 1200, 1201, 1202, 1203, 1204, 1205]);
/// `#minecraft:copper_golem_statues`.
pub static COPPER_GOLEM_STATUES: Tag = Tag(&[1206, 1207, 1208, 1209, 1210, 1211, 1212, 1213]);
/// `#minecraft:coral_blocks`.
pub static CORAL_BLOCKS: Tag = Tag(&[843, 844, 845, 846, 847]);
/// `#minecraft:cushion_uses_collision_shape`.
pub static CUSHION_USES_COLLISION_SHAPE: Tag = Tag(&[437, 438, 439, 440, 530, 999]);
/// `#minecraft:dirt`.
pub static DIRT: Tag = Tag(&[9, 10, 1239]);
/// `#minecraft:enables_bubble_column_drag_down`.
pub static ENABLES_BUBBLE_COLUMN_DRAG_DOWN: Tag = Tag(&[729]);
/// `#minecraft:enables_bubble_column_push_up`.
pub static ENABLES_BUBBLE_COLUMN_PUSH_UP: Tag = Tag(&[335]);
/// `#minecraft:enchantment_power_provider`.
pub static ENCHANTMENT_POWER_PROVIDER: Tag = Tag(&[221]);
/// `#minecraft:enchantment_power_transmitter`.
pub static ENCHANTMENT_POWER_TRANSMITTER: Tag = Tag(&[
    0, 37, 38, 140, 141, 142, 143, 144, 145, 146, 147, 148, 240, 241, 325, 416, 417, 418, 579, 615,
    616, 733, 884, 885, 886, 959, 960, 972, 1233, 1238,
]);
/// `#minecraft:fences`.
pub static FENCES: Tag = Tag(&[
    333, 432, 692, 693, 694, 695, 696, 697, 698, 699, 700, 701, 979, 980,
]);
/// `#minecraft:fire`.
pub static FIRE: Tag = Tag(&[240, 241]);
/// `#minecraft:grows_crops`.
pub static GROWS_CROPS: Tag = Tag(&[252]);
/// `#minecraft:guarded_by_piglins`.
pub static GUARDED_BY_PIGLINS: Tag = Tag(&[
    44, 45, 50, 217, 245, 451, 523, 735, 736, 737, 738, 739, 740, 741, 742, 743, 744, 745, 746,
    747, 748, 749, 750, 751, 929, 1025, 1198, 1199, 1200, 1201, 1202, 1203, 1204, 1205, 1265,
]);
/// `#minecraft:ice_melts_when_destroyed_above`.
pub static ICE_MELTS_WHEN_DESTROYED_ABOVE: Tag = Tag(&[
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 36,
    39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62,
    63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86,
    87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107,
    108, 109, 110, 111, 112, 113, 114, 115, 116, 117, 118, 119, 120, 121, 122, 123, 124, 125, 126,
    127, 128, 129, 130, 131, 132, 133, 134, 135, 138, 149, 150, 151, 152, 153, 154, 155, 156, 157,
    158, 159, 160, 161, 162, 163, 164, 165, 166, 167, 168, 169, 170, 171, 172, 173, 174, 175, 176,
    177, 178, 179, 180, 181, 182, 183, 184, 185, 186, 187, 188, 189, 190, 191, 192, 193, 194, 195,
    196, 197, 198, 199, 217, 218, 219, 220, 221, 222, 223, 224, 225, 226, 227, 228, 229, 230, 231,
    232, 233, 234, 235, 236, 237, 242, 243, 244, 245, 247, 248, 249, 250, 252, 253, 265, 268, 280,
    281, 282, 283, 284, 285, 286, 287, 288, 289, 290, 291, 292, 293, 294, 295, 296, 297, 298, 299,
    300, 301, 302, 303, 304, 305, 307, 308, 309, 310, 311, 312, 313, 314, 315, 316, 317, 318, 319,
    320, 321, 326, 327, 328, 330, 332, 333, 334, 335, 336, 337, 338, 343, 345, 346, 347, 349, 350,
    351, 352, 353, 354, 355, 356, 357, 358, 359, 360, 361, 362, 363, 364, 365, 366, 367, 368, 369,
    370, 371, 372, 373, 374, 375, 376, 377, 378, 379, 380, 381, 382, 383, 384, 385, 386, 387, 388,
    389, 390, 391, 392, 393, 394, 395, 396, 397, 398, 399, 400, 401, 402, 403, 404, 405, 406, 407,
    408, 409, 410, 411, 419, 420, 421, 422, 423, 425, 426, 427, 428, 429, 430, 431, 432, 433, 435,
    436, 437, 438, 439, 440, 442, 443, 444, 445, 448, 449, 450, 451, 454, 455, 456, 457, 458, 459,
    460, 461, 520, 521, 522, 523, 524, 525, 527, 528, 529, 530, 531, 532, 533, 534, 536, 537, 538,
    539, 540, 541, 542, 543, 544, 545, 546, 547, 548, 549, 550, 551, 552, 553, 554, 555, 556, 557,
    558, 559, 560, 561, 562, 563, 564, 565, 566, 567, 568, 569, 570, 571, 572, 573, 574, 575, 576,
    577, 578, 580, 581, 582, 583, 584, 585, 586, 587, 588, 589, 590, 591, 608, 609, 610, 617, 618,
    619, 620, 621, 622, 623, 624, 625, 626, 627, 628, 629, 630, 631, 632, 633, 634, 635, 636, 637,
    638, 639, 640, 641, 642, 643, 644, 645, 646, 647, 648, 649, 650, 651, 652, 653, 654, 655, 656,
    657, 658, 659, 660, 661, 662, 663, 664, 665, 666, 667, 668, 669, 670, 671, 672, 673, 674, 675,
    676, 677, 678, 679, 680, 681, 682, 683, 684, 685, 686, 687, 688, 689, 690, 691, 692, 693, 694,
    695, 696, 697, 698, 699, 700, 701, 702, 703, 704, 705, 706, 707, 708, 709, 710, 711, 715, 716,
    717, 718, 719, 724, 726, 727, 728, 729, 730, 731, 732, 734, 735, 736, 737, 738, 739, 740, 741,
    742, 743, 744, 745, 746, 747, 748, 749, 750, 751, 752, 753, 754, 755, 756, 757, 758, 759, 760,
    761, 762, 763, 764, 765, 766, 767, 768, 769, 770, 771, 772, 773, 774, 775, 776, 777, 778, 779,
    780, 781, 782, 783, 784, 785, 786, 787, 788, 789, 790, 791, 792, 793, 794, 795, 796, 797, 798,
    799, 800, 801, 802, 803, 804, 805, 806, 807, 808, 809, 810, 811, 812, 813, 814, 815, 816, 817,
    818, 819, 820, 821, 822, 823, 824, 825, 826, 827, 828, 829, 830, 831, 834, 835, 836, 837, 838,
    839, 840, 841, 842, 843, 844, 845, 846, 847, 848, 849, 850, 851, 852, 858, 859, 860, 861, 862,
    868, 869, 870, 871, 872, 879, 880, 882, 887, 888, 889, 890, 891, 892, 893, 894, 895, 896, 897,
    898, 899, 900, 901, 902, 903, 904, 905, 906, 907, 908, 909, 910, 911, 912, 913, 914, 915, 916,
    917, 918, 919, 920, 921, 922, 923, 924, 925, 926, 928, 929, 930, 931, 932, 933, 934, 935, 936,
    937, 938, 939, 940, 941, 942, 943, 944, 945, 946, 947, 948, 949, 950, 952, 953, 954, 955, 956,
    958, 961, 962, 963, 964, 965, 967, 973, 974, 975, 976, 977, 978, 979, 980, 981, 982, 983, 984,
    985, 986, 989, 990, 995, 996, 997, 998, 999, 1000, 1001, 1002, 1003, 1004, 1005, 1006, 1007,
    1008, 1013, 1014, 1015, 1016, 1017, 1018, 1019, 1020, 1021, 1022, 1023, 1024, 1025, 1026, 1027,
    1028, 1030, 1031, 1032, 1033, 1051, 1052, 1053, 1054, 1055, 1056, 1057, 1058, 1059, 1060, 1061,
    1062, 1063, 1064, 1065, 1066, 1067, 1068, 1069, 1070, 1071, 1072, 1073, 1074, 1075, 1076, 1077,
    1078, 1079, 1080, 1081, 1082, 1083, 1084, 1085, 1086, 1087, 1088, 1089, 1090, 1091, 1092, 1093,
    1094, 1095, 1096, 1097, 1098, 1099, 1100, 1101, 1102, 1103, 1104, 1105, 1106, 1107, 1108, 1109,
    1110, 1111, 1112, 1113, 1114, 1115, 1116, 1118, 1119, 1120, 1121, 1122, 1123, 1124, 1125, 1126,
    1127, 1128, 1129, 1130, 1131, 1132, 1133, 1134, 1135, 1136, 1137, 1138, 1139, 1140, 1141, 1142,
    1143, 1144, 1145, 1146, 1147, 1148, 1149, 1150, 1151, 1152, 1153, 1154, 1155, 1156, 1157, 1158,
    1159, 1160, 1161, 1162, 1163, 1164, 1165, 1166, 1167, 1168, 1169, 1170, 1171, 1172, 1173, 1174,
    1175, 1176, 1177, 1178, 1179, 1180, 1181, 1182, 1183, 1184, 1185, 1186, 1187, 1188, 1189, 1190,
    1191, 1192, 1193, 1194, 1195, 1196, 1197, 1198, 1199, 1200, 1201, 1202, 1203, 1204, 1205, 1214,
    1215, 1216, 1217, 1218, 1219, 1220, 1221, 1222, 1223, 1224, 1234, 1239, 1240, 1241, 1242, 1243,
    1244, 1245, 1246, 1247, 1248, 1249, 1250, 1251, 1252, 1253, 1254, 1255, 1256, 1257, 1258, 1259,
    1260, 1261, 1262, 1263, 1264, 1265, 1268, 1269, 1270, 1272, 1273, 1274, 1275, 1276, 1278,
]);
/// `#minecraft:impermeable`.
pub static IMPERMEABLE: Tag = Tag(&[
    110, 349, 350, 351, 352, 353, 354, 355, 356, 357, 358, 359, 360, 361, 362, 363, 364, 578, 1116,
]);
/// `#minecraft:leaves`.
pub static LEAVES: Tag = Tag(&[
    94, 95, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107,
]);
/// `#minecraft:logs`.
pub static LOGS: Tag = Tag(&[
    20, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 75, 76, 77,
    78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 952, 953, 954, 955, 961, 962,
    963, 964,
]);
/// `#minecraft:maintains_farmland`.
pub static MAINTAINS_FARMLAND: Tag = Tag(&[
    199, 202, 251, 412, 413, 414, 415, 419, 493, 494, 682, 683, 684, 685, 686, 687, 688, 689, 690,
    691, 720, 721, 723, 983, 984,
]);
/// `#minecraft:nylium`.
pub static NYLIUM: Tag = Tag(&[956, 965]);
/// `#minecraft:overrides_mushroom_light_requirement`.
pub static OVERRIDES_MUSHROOM_LIGHT_REQUIREMENT: Tag = Tag(&[11, 423, 956, 965]);
/// `#minecraft:pale_oak_logs`.
pub static PALE_OAK_LOGS: Tag = Tag(&[20, 58, 70, 91]);
/// `#minecraft:prevents_nearby_leaf_decay`.
pub static PREVENTS_NEARBY_LEAF_DECAY: Tag = Tag(&[
    20, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 75, 76, 77,
    78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 952, 953, 954, 955, 961, 962,
    963, 964,
]);
/// `#minecraft:rails`.
pub static RAILS: Tag = Tag(&[136, 137, 267, 535]);
/// `#minecraft:required_for_poplar_leaf_ambience`.
pub static REQUIRED_FOR_POPLAR_LEAF_AMBIENCE: Tag = Tag(&[51, 52, 53, 54, 55, 56, 57, 58, 59, 60]);
/// `#minecraft:sand`.
pub static SAND: Tag = Tag(&[39, 40, 41]);
/// `#minecraft:sculk_growth_inhibitors`.
pub static SCULK_GROWTH_INHIBITORS: Tag = Tag(&[1118, 1123]);
/// `#minecraft:sculk_replaceable`.
pub static SCULK_REPLACEABLE: Tag = Tag(&[
    1, 2, 4, 6, 8, 9, 10, 11, 39, 41, 42, 62, 115, 330, 334, 335, 336, 337, 423, 443, 537, 538,
    539, 540, 541, 542, 543, 544, 545, 546, 547, 548, 549, 550, 551, 552, 608, 649, 956, 965, 1014,
    1074, 1088, 1102, 1115, 1222, 1234, 1239, 1240, 1241, 1262, 1278,
]);
/// `#minecraft:sculk_replaceable_world_gen`.
pub static SCULK_REPLACEABLE_WORLD_GEN: Tag = Tag(&[
    1, 2, 4, 6, 8, 9, 10, 11, 39, 41, 42, 62, 115, 330, 334, 335, 336, 337, 423, 443, 537, 538,
    539, 540, 541, 542, 543, 544, 545, 546, 547, 548, 549, 550, 551, 552, 608, 649, 956, 965, 1014,
    1074, 1088, 1102, 1115, 1222, 1234, 1239, 1240, 1241, 1242, 1246, 1250, 1254, 1259, 1260, 1262,
    1278,
]);
/// `#minecraft:shears_extreme_breaking_speed`.
pub static SHEARS_EXTREME_BREAKING_SPEED: Tag = Tag(&[
    94, 95, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107,
]);
/// `#minecraft:shears_major_breaking_speed`.
pub static SHEARS_MAJOR_BREAKING_SPEED: Tag = Tag(&[
    151, 152, 153, 154, 155, 156, 157, 158, 159, 160, 161, 162, 163, 164, 165, 166, 167, 168, 169,
    170, 171, 172, 173, 174, 175, 176, 177, 178, 179, 180, 181, 182, 183, 184, 185, 186, 187, 188,
    189, 190, 191, 192, 193, 194, 195, 196, 197, 198,
]);
/// `#minecraft:shears_minor_breaking_speed`.
pub static SHEARS_MINOR_BREAKING_SPEED: Tag = Tag(&[416, 417]);
/// `#minecraft:shulker_boxes`.
pub static SHULKER_BOXES: Tag = Tag(&[
    735, 736, 737, 738, 739, 740, 741, 742, 743, 744, 745, 746, 747, 748, 749, 750, 751,
]);
/// `#minecraft:sniffer_egg_hatch_boost`.
pub static SNIFFER_EGG_HATCH_BOOST: Tag = Tag(&[1234]);
/// `#minecraft:snow`.
pub static SNOW: Tag = Tag(&[325, 327, 1117]);
/// `#minecraft:soul_fire_base_blocks`.
pub static SOUL_FIRE_BASE_BLOCKS: Tag = Tag(&[335, 336]);
/// `#minecraft:speleothems`.
pub static SPELEOTHEMS: Tag = Tag(&[1223, 1224]);
/// `#minecraft:support_override_cactus_flower`.
pub static SUPPORT_OVERRIDE_CACTUS_FLOWER: Tag = Tag(&[252, 328]);
/// `#minecraft:support_override_snow_layer`.
pub static SUPPORT_OVERRIDE_SNOW_LAYER: Tag = Tag(&[335, 1003, 1240]);
/// `#minecraft:supports_azalea`.
pub static SUPPORTS_AZALEA: Tag = Tag(&[8, 9, 10, 11, 62, 252, 330, 423, 1234, 1239, 1240, 1278]);
/// `#minecraft:supports_bamboo`.
pub static SUPPORTS_BAMBOO: Tag = Tag(&[
    8, 9, 10, 11, 39, 40, 41, 42, 43, 62, 423, 881, 882, 1234, 1239, 1240, 1278,
]);
/// `#minecraft:supports_big_dripleaf`.
pub static SUPPORTS_BIG_DRIPLEAF: Tag = Tag(&[8, 9, 10, 11, 62, 252, 330, 423, 1234, 1239, 1240]);
/// `#minecraft:supports_cactus`.
pub static SUPPORTS_CACTUS: Tag = Tag(&[39, 40, 41]);
/// `#minecraft:supports_chorus_flower`.
pub static SUPPORTS_CHORUS_FLOWER: Tag = Tag(&[443]);
/// `#minecraft:supports_chorus_plant`.
pub static SUPPORTS_CHORUS_PLANT: Tag = Tag(&[443]);
/// `#minecraft:supports_cocoa`.
pub static SUPPORTS_COCOA: Tag = Tag(&[54, 66, 78, 87]);
/// `#minecraft:supports_crimson_fungus`.
pub static SUPPORTS_CRIMSON_FUNGUS: Tag = Tag(&[
    8, 9, 10, 11, 62, 252, 336, 423, 956, 965, 1234, 1239, 1240, 1278,
]);
/// `#minecraft:supports_crimson_roots`.
pub static SUPPORTS_CRIMSON_ROOTS: Tag = Tag(&[
    8, 9, 10, 11, 62, 252, 336, 423, 956, 965, 1234, 1239, 1240, 1278,
]);
/// `#minecraft:supports_crops`.
pub static SUPPORTS_CROPS: Tag = Tag(&[252]);
/// `#minecraft:supports_dry_vegetation`.
pub static SUPPORTS_DRY_VEGETATION: Tag = Tag(&[
    8, 9, 10, 11, 39, 40, 41, 62, 252, 423, 537, 538, 539, 540, 541, 542, 543, 544, 545, 546, 547,
    548, 549, 550, 551, 552, 608, 1234, 1239, 1240, 1278,
]);
/// `#minecraft:supports_frogspawn`.
pub static SUPPORTS_FROGSPAWN: Tag = Tag(&[]);
/// `#minecraft:supports_hanging_mangrove_propagule`.
pub static SUPPORTS_HANGING_MANGROVE_PROPAGULE: Tag = Tag(&[102]);
/// `#minecraft:supports_lily_pad`.
pub static SUPPORTS_LILY_PAD: Tag = Tag(&[326, 728]);
/// `#minecraft:supports_mangrove_propagule`.
pub static SUPPORTS_MANGROVE_PROPAGULE: Tag =
    Tag(&[8, 9, 10, 11, 62, 252, 330, 423, 1234, 1239, 1240, 1278]);
/// `#minecraft:supports_melon_stem`.
pub static SUPPORTS_MELON_STEM: Tag = Tag(&[252]);
/// `#minecraft:supports_melon_stem_fruit`.
pub static SUPPORTS_MELON_STEM_FRUIT: Tag =
    Tag(&[8, 9, 10, 11, 62, 252, 423, 1234, 1239, 1240, 1278]);
/// `#minecraft:supports_nether_sprouts`.
pub static SUPPORTS_NETHER_SPROUTS: Tag = Tag(&[
    8, 9, 10, 11, 62, 252, 336, 423, 956, 965, 1234, 1239, 1240, 1278,
]);
/// `#minecraft:supports_nether_wart`.
pub static SUPPORTS_NETHER_WART: Tag = Tag(&[335]);
/// `#minecraft:supports_pumpkin_stem`.
pub static SUPPORTS_PUMPKIN_STEM: Tag = Tag(&[252]);
/// `#minecraft:supports_pumpkin_stem_fruit`.
pub static SUPPORTS_PUMPKIN_STEM_FRUIT: Tag =
    Tag(&[8, 9, 10, 11, 62, 252, 423, 1234, 1239, 1240, 1278]);
/// `#minecraft:supports_small_dripleaf`.
pub static SUPPORTS_SMALL_DRIPLEAF: Tag = Tag(&[330, 1234]);
/// `#minecraft:supports_sugar_cane`.
pub static SUPPORTS_SUGAR_CANE: Tag =
    Tag(&[8, 9, 10, 11, 39, 40, 41, 62, 423, 1234, 1239, 1240, 1278]);
/// `#minecraft:supports_sugar_cane_adjacently`.
pub static SUPPORTS_SUGAR_CANE_ADJACENTLY: Tag = Tag(&[728]);
/// `#minecraft:supports_vegetation`.
pub static SUPPORTS_VEGETATION: Tag = Tag(&[8, 9, 10, 11, 62, 252, 423, 1234, 1239, 1240, 1278]);
/// `#minecraft:supports_warped_fungus`.
pub static SUPPORTS_WARPED_FUNGUS: Tag = Tag(&[
    8, 9, 10, 11, 62, 252, 336, 423, 956, 965, 1234, 1239, 1240, 1278,
]);
/// `#minecraft:supports_warped_roots`.
pub static SUPPORTS_WARPED_ROOTS: Tag = Tag(&[
    8, 9, 10, 11, 62, 252, 336, 423, 956, 965, 1234, 1239, 1240, 1278,
]);
/// `#minecraft:supports_wither_rose`.
pub static SUPPORTS_WITHER_ROSE: Tag = Tag(&[
    8, 9, 10, 11, 62, 252, 334, 335, 336, 423, 1234, 1239, 1240, 1278,
]);
/// `#minecraft:triggers_ambient_dried_ghast_block_sounds`.
pub static TRIGGERS_AMBIENT_DRIED_GHAST_BLOCK_SOUNDS: Tag = Tag(&[335, 336]);
/// `#minecraft:underwater_bonemeals`.
pub static UNDERWATER_BONEMEALS: Tag = Tag(&[
    147, 853, 854, 855, 856, 857, 863, 864, 865, 866, 867, 873, 874, 875, 876, 877,
]);
/// `#minecraft:unstable_bottom_center`.
pub static UNSTABLE_BOTTOM_CENTER: Tag = Tag(&[
    419, 682, 683, 684, 685, 686, 687, 688, 689, 690, 691, 983, 984,
]);
/// `#minecraft:vibration_resonators`.
pub static VIBRATION_RESONATORS: Tag = Tag(&[1068]);
/// `#minecraft:wall_corals`.
pub static WALL_CORALS: Tag = Tag(&[873, 874, 875, 876, 877]);
/// `#minecraft:wall_hanging_signs`.
pub static WALL_HANGING_SIGNS: Tag = Tag(&[
    293, 294, 295, 296, 297, 298, 299, 300, 301, 302, 303, 304, 305,
]);
/// `#minecraft:wall_post_override`.
pub static WALL_POST_OVERRIDE: Tag = Tag(&[
    238, 254, 255, 256, 257, 258, 259, 260, 261, 262, 263, 264, 269, 270, 271, 272, 273, 274, 275,
    276, 277, 278, 279, 307, 309, 310, 311, 312, 313, 314, 315, 316, 317, 318, 319, 322, 329, 339,
    341, 453, 524, 525, 617, 618, 619, 620, 621, 622, 623, 624, 625, 626, 627, 628, 629, 630, 631,
    632, 633, 634, 635, 636, 637, 638, 639, 640, 641, 642, 643, 644, 645, 646, 647, 648, 977, 978,
    991, 992, 993, 994, 1028,
]);
/// `#minecraft:walls`.
pub static WALLS: Tag = Tag(&[
    429, 460, 461, 914, 915, 916, 917, 918, 919, 920, 921, 922, 923, 924, 925, 926, 1016, 1024,
    1030, 1077, 1081, 1086, 1092, 1096, 1100, 1105, 1109, 1113, 1245, 1249, 1253, 1257,
]);
/// `#minecraft:wither_summon_base_blocks`.
pub static WITHER_SUMMON_BASE_BLOCKS: Tag = Tag(&[335, 336]);
/// `#minecraft:wooden_fences`.
pub static WOODEN_FENCES: Tag = Tag(&[
    333, 692, 693, 694, 695, 696, 697, 698, 699, 700, 701, 979, 980,
]);
/// `#minecraft:wooden_shelves`.
pub static WOODEN_SHELVES: Tag = Tag(&[
    223, 224, 225, 226, 227, 228, 229, 230, 231, 232, 233, 234, 235,
]);
