import { Box, Result as R } from "./types";

@sealed
export class Service<T extends Box<Box<string>>> {
  private readonly label = "{ // }";

  async load<U extends Box<Box<T>>>(
    value: U,
    count: number,
  ): Promise<T> {
    const message = `value ${format({ nested: count })} // ${`${count}`}`;
    return this.convert(value);
  }

  convert(value: unknown): T {
    return parse(value);
  }
}

export const make = (
  value: string,
) => create(value);
