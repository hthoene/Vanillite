// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import net.minecraft.util.RandomSource;
import net.minecraft.world.entity.Entity;

/**
 * The seeds {@link ParityAgent} puts into Vanilla.
 *
 * <p>Entity seeds (the contract Vanillite's scenario world follows, see
 * {@code crates/vanillite-test/src/scenario.rs}):
 *
 * <ul>
 *   <li>a scenario entity is created with its own {@code seed};
 *   <li>every entity created while the scenario runs (arrows, items, experience orbs, babies,
 *       naturally spawned mobs) takes the next {@code nextLong()} of a legacy random seeded with
 *       the scenario's {@code entity_seed}, in creation order;
 *   <li>the seed replaces the entity's random at the end of the {@code Entity} constructor.
 * </ul>
 */
public final class ParityHooks {
  /** The seed of every shuffled behaviour list's random while a scenario runs. */
  static final long SHUFFLE_SEED = 0x5348_5546L;

  private static long unique;
  private static boolean scenario;
  private static RandomSource stream;
  private static Long next;

  private ParityHooks() {}

  /** Replaces {@code RandomSupport.generateUniqueSeed()}: the same sequence on every run. */
  public static synchronized long uniqueSeed() {
    unique++;
    return unique * 0x9E3779B97F4A7C15L;
  }

  /**
   * Replaces {@code RandomSource.create()} in the {@code ShufflingList} constructors: while a
   * scenario runs, every list's random starts from {@link #SHUFFLE_SEED}, so the shuffles of
   * gate behaviours do not depend on the order the lists were created in. The unique seed is
   * still taken, so the seeds of all other randoms stay the same.
   */
  public static synchronized RandomSource shuffleRandom() {
    long seed = uniqueSeed();
    return RandomSource.create(scenario ? SHUFFLE_SEED : seed);
  }

  /**
   * Replaces {@code Collections.shuffle(list)}, which draws from an unseeded JVM wide random:
   * while a scenario runs, every such shuffle uses a new {@code java.util.Random} seeded with
   * {@link #SHUFFLE_SEED}, as Vanillite's scenario world does.
   */
  public static synchronized void shuffle(java.util.List<?> list) {
    if (scenario) {
      java.util.Collections.shuffle(list, new java.util.Random(SHUFFLE_SEED));
    } else {
      java.util.Collections.shuffle(list);
    }
  }

  /** Starts a scenario: unique seeds from the start, no entity stream yet. */
  public static synchronized void reset() {
    scenario = true;
    unique = 0;
    stream = null;
    next = null;
  }

  /** The seed of the next entity created (a scenario entity). */
  public static synchronized void seedNext(long seed) {
    next = seed;
  }

  /** From now on, new entities take their seeds from a stream seeded with {@code seed}. */
  public static synchronized void startStream(long seed) {
    stream = RandomSource.create(seed);
  }

  /** Called at the end of the {@code Entity} constructor. */
  public static synchronized void entityCreated(Object entity) {
    long seed;
    if (next != null) {
      seed = next;
      next = null;
    } else if (stream != null) {
      seed = stream.nextLong();
    } else {
      return;
    }
    ((Entity) entity).getRandom().setSeed(seed);
  }
}
